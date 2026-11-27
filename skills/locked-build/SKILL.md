---
name: locked-build
description: Run or inspect shared-host build leases with locked-build. Use when coordinating Cargo builds, checking queued work, or diagnosing memory-cap outcomes.
---

# locked-build

Use the installed `locked-build` command to submit and inspect builds. Do not write a private wrapper around it.

## Run a command

```sh
locked-build run -- cargo test -p package-name
```

State, policy, and generated targets default to `<ix-home>/locked-build/{state,policy.json,targets}`. IX home resolves exactly as Quoin: nonempty `IX_HOME`, then `$HOME/.ix`, then `.ix`. State is an owned alias to the shared, owned mode-0700 `/tmp/locked-build-<uid>` queue, also used directly when IX home is read-only; conflicting existing state paths are refused. Absent policy is generated in that same directory, with an IX home alias when writable, so writable and read-only lanes share both identities. Unwritable generated targets also fall back there. Existing policy requires only read access. `status` reports `state_path`; `policy show` prints effective paths to stderr. The first invocation writes a measured host policy. Use `--state` or `--config` only to override the defaults. `install-tools` moves the old `~/.config/locked-build/policy.json` into IX home while preserving hand-tuned settings, and refuses conflicting old/new policies. Nested Cargo calls pass directly to real Cargo without state access when `LOCKED_BUILD_LEASE=1`, or when the legacy runner sets a nonempty `CARGO_TARGET_DIR` and equal positive `CARGO_BUILD_JOBS`/`RUST_TEST_THREADS`.

Choose `--class must-fix`, `--class point`, `--class block`, or `--class periodic`. The default is `periodic`. Direct runs and the Cargo shim derive the owner from the user, host, and current worktree directory; `LOCKED_BUILD_OWNER` overrides it. Ticket labels derive from the Git branch; `LOCKED_BUILD_TICKET` overrides them. This gives each worktree its own Cargo target directory.

## Read request status

```sh
locked-build status
locked-build cancel REQUEST-ID
```

`queued` means the request is accepted and waiting for admission. Its command has not started. `running` means it owns a lease and its command has started. Use the request id from status to cancel queued or running work.

The generated `hold_ms` default is 5,400,000 ms (90 minutes), the ceiling for
any command; tune it in each host's policy. If the limit ends a command,
`locked-build status` and the `run` output name the limit and direct you to
change policy `hold_ms`.

`rss_cap` means memory enforcement killed the command for exceeding its cap. `memory_cap_unavailable` means policy required memory enforcement that the launcher could not establish, so the command was not launched.

Each refusal or abnormal completion prints one diagnostic line to the caller
and saves that same text as `message` in the event ledger and status JSON:

```text
locked-build: <CODE> request=<id> limit=<policy key>=<value> measured=<value> (<what>) killed=<pid/command>
```

| Code | Limit source and measurement |
|---|---|
| `LB_RSS_CAP` | `policy.memory_per_job_bytes*parallelism` (or the class override); measured process-group RSS bytes |
| `LB_OOM_KILLED` | The same memory cap; evidence from the scope's `memory.events` `oom_kill` counter |
| `LB_HOLD_TIMEOUT` | `policy.hold_ms`; elapsed lease milliseconds |
| `LB_SUPERVISION_LOST` | Built-in `policy.supervision_required=true`; worker/watchdog evidence, or degraded supervision with observed exit 0 |
| `LB_REFUSED_MEMORY_BUDGET` | `policy.memory_budget_bytes`; requested lease bytes |
| `LB_REFUSED_CLASS_CAP` | `policy.memory_cap_by_class.<class>`; requested lease bytes |
| `LB_REFUSED_QUEUE` | `policy.MAX_REQUESTS` or `policy.maximum_units`; retained requests or requested units |
| `LB_REFUSED_CAP_UNAVAILABLE` | `policy.on_missing_cgroup`; unavailable cap backend |
| `LB_CANCELLED` | Built-in `policy.run_permitted=true`; caller revoked permission |
| `LB_EXTERNAL_SIGNAL` | Built-in `policy.external_signals=none`; “terminated by signal N from outside locked-build” |
| `LB_COMMAND_EXIT` | Built-in `policy.success_exit_code=0`; actual command exit status |
| `LB_INVALID_REQUEST` | Built-in `policy.valid_request=true` or configured priority bound; rejected input |
| `LB_STATE_UNAVAILABLE` | Built-in `policy.state_access=read_write`; unavailable state/configuration |

Built-in rule names describe required behavior; they are not configurable
policy keys. An unavailable historical measurement is explicit. `killed`
identifies the process group or `not-started/command` for a refusal.
`locked-build error-codes` lists the enum. `code`, `limit_key`, `limit_value`,
`measured_value`, `measured_what` and `killed` are separate JSON fields.
When the worker can continue supervising after a watchdog glitch, it records
the real command exit status. A successful command stays successful and reports
that supervision was degraded. Linux reboot recovery uses the kernel boot UUID;
macOS uses `kern.boottime`. Timestamp-only fallback tolerates 120 seconds of
jitter, so small clock changes do not expire live leases.

Queued requests expose `memory_needed_bytes`, `memory_free_budget_bytes`,
`position`, `queue_depth_at_enqueue`, requested/granted jobs and memory,
per-state timestamps/durations, and cumulative `blocked_reasons` with observed
value, limit and seconds. Reasons can overlap. Memory-blocked heads allow
fitting backfill until `(maximum priority + 1) * aging_ms`, then reserve
admission to prevent starvation. Must-fix always precedes aged lower classes.
Backfill never exceeds the memory budget.

`metrics` exports `ix_build_requests_total{host,class,outcome}`,
`ix_build_terminations_total{host,class,code}`,
`ix_build_queue_wait_seconds{host,class}` (histogram), and
`ix_build_blocked_seconds_total{host,class,reason}`. Status JSON's `metrics`
object carries the same families. Counts survive reopening and explicit
pruning; no request ids or owners become metric labels.

Use `locked-build policy show` to inspect the effective policy. `locked-build policy init --force` regenerates it.

Tool manifest entries without `version` require executable presence; `check-install` runs `--help` and passes only when it exits successfully.

An explicit `CARGO_TARGET_DIR` is preserved by `locked-build run` and the Cargo shim, is excluded from per-lease trimming, and is cleaned by worktree hygiene.

## Install and verify a host

From the dev-tools repository root, install the binary, measured policy, and
Cargo shim with `make install-tools`. The command is idempotent and puts the
shim ahead of real Cargo in the supported shell startup files. Start a new
shell after the first install. npm packages are installed with both
`--registry=https://registry.npmjs.org/` and
`--@agent-ix:registry=https://registry.npmjs.org/`, so a user or scope registry
setting cannot route Agent IX package installs through a different registry.
Run `locked-build check-install` to compare installed CLI pins against
`tools/locked-build/tool-versions.json`; its one-line `ok`/`mismatch` output
and exit status are suitable for ix-board cadence checks. This host manifest
does not pin Cargo or rustc; each repository owns its toolchain selection,
using `rust-toolchain` where configured. A `rust-version` value sets an MSRV
floor, not a host-wide toolchain pin.

## Wrapper migration map

| Old command or state | New command |
|---|---|
| `locked-build.sh` build lane | `locked-build run --class <must-fix\|point\|block\|periodic> --owner <owner> --ticket <ticket> -- cargo ...` |
| `locked-build.sh` Kani lane | `locked-build run --class kani --owner <owner> --ticket <ticket> -- cargo kani ...` |
| `lead-reqs-cargo.sh` | `locked-build run --class must-fix --owner lead-reqs --ticket <ticket> -- cargo ...` |
| `arch-test-run.sh` | `locked-build worktree-run --repository <repo> --slot <n> --ticket <ticket> -- cargo test ...` |
| cargo-resource-controller jobs registry | `locked-build status` and `locked-build metrics` |
| per-lane `gate.sh` or raw `timeout ... cargo` | `locked-build run --class <class> --owner <owner> --ticket <ticket> -- cargo ...` |

The checked-in host capacity examples record core and RAM measurements for
draco (18 cores), luna (8), titan (32), and terra (14). Installation uses the
host's measured cores, RAM, and disk when
writing its local policy. Terra's Linux verification is run through `ssh cave`.
Windows is unsupported: the process supervisor uses Unix process and signal
APIs.
