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

Every lane of a user shares `/tmp/locked-build-<uid>` (owned, mode `0700`) and its `policy.json`, regardless of lane `IX_HOME`. `~/.ix/locked-build/state` and `policy.json` are host aliases. The canonical policy is read if present and created atomically once if absent; generated host targets default to `~/.ix/locked-build/targets`, with a writable shared fallback. Explicit state/config overrides remain available for isolated fixtures. `status` reports `state_path`; `policy show` prints effective paths to stderr. Installation preserves the policy selected by existing host state, moves old `~/.config/locked-build/policy.json` when applicable, and archives a competing fallback policy before host migration. Nested Cargo calls pass directly to real Cargo without state access when `LOCKED_BUILD_LEASE=1`, or when the legacy runner sets a nonempty `CARGO_TARGET_DIR` and equal positive `CARGO_BUILD_JOBS`/`RUST_TEST_THREADS`.

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

Generated policies reserve `memory_reserve_per_job_bytes=3000000000` (3 GB)
per effective worker for admission, and enforce
`memory_limit_per_job_bytes=8000000000` (8 GB) per worker as a hard cap.
`memory_reserve_per_job_by_class` overrides reservations;
`memory_per_job_by_class` overrides hard limits. The sum of admitted
reservations must fit `memory_budget_bytes` (measured host RAM minus the host
reserve). Hard caps can sum above that budget while each command stays capped.
For `-j2`, the defaults reserve 6 GB and cap at 16 GB, so an 8.3 GB peak fits.
At a 49.4 GB budget, five default `-j3` reservations consume 45 GB; six would
need 54 GB. A 2.5 GB/job class reservation allows six (45 GB), with 24 GB hard
caps unchanged. This replaces the earlier combined 6 GB/job default.

`rss_cap` means memory enforcement killed the command for exceeding its cap, including a cgroup scope whose `memory.events` reports `oom_kill`. Status includes `memory_cap_bytes`; `run` reports the request id, `rss_cap`, and cap in bytes. The observed child signal is retained as diagnostic data. `memory_cap_unavailable` means policy required memory enforcement that the launcher could not establish, so the command was not launched.

`supervision_lost` means the runner lost its watchdog, scope, or lease supervision. It terminates the whole command group with TERM and then SIGKILL after `termination_grace_ms` before returning. Linux recovery uses the kernel boot UUID so a drifting boot timestamp cannot expire a live lease.

`supervisor_lost` is recovery's label when its worker is absent/different and both owned groups are verified gone, without a recorded command outcome. It is not an observed command exit code. PIDs are namespace-local: recovery in another PID namespace retains the lease and never infers death or respawns its worker from those numbers. `install-tools` pins the host PID namespace and boot UUID once per boot. Only that host namespace may reclaim an extinct foreign namespace, after a complete kernel procfs scan proves absence; unreadable or racing entries retain the reservation with reason `foreign_pid_namespace_live_or_unverifiable`. Run `install-tools` in the host namespace, then host-side `recover`. Extinction recovery requires permission to read every `/proc/*/ns/pid` link; on terra an ordinary user cannot read `/proc/1/ns/pid`, so an operator must provide that read access (for example a host recovery process retaining the store owner's UID with `CAP_SYS_PTRACE`). Incomplete evidence never releases capacity. Legacy queued requests with no worker can safely adopt the current namespace.

The fallback watchdog counts only the launched command and its descendants, validated against the command's birth identity. Linux thread tasks are excluded so each address space is counted once. Linux uses summed `Pss_Anon` bytes from `/proc/<pid>/smaps_rollup`, proportionally accounting shared anonymous pages and excluding file/tmpfs mappings; macOS uses RSS. Each lease logs one initial sample with contributing PIDs/bytes; the sample that crosses the cap is saved in `lease_record.memory_sample`. A cap diagnostic includes measured bytes, cap bytes, and metric. Cgroup OOM diagnostics use `memory.peak` when available.

Each refusal or abnormal completion prints one diagnostic line to the caller
and saves that same text as `message` in the event ledger and status JSON:

```text
locked-build: <CODE> request=<id> limit=<policy key>=<value> measured=<value> (<what>) killed=<pid/command>
```

| Code | Limit source and measurement |
|---|---|
| `LB_RSS_CAP` | `policy.memory_limit_per_job_bytes*parallelism` (or the class override); measured command-tree `pss_anon` bytes on Linux, RSS on macOS |
| `LB_OOM_KILLED` | The same memory cap; evidence from the scope's `memory.events` `oom_kill` counter |
| `LB_HOLD_TIMEOUT` | `policy.hold_ms`; elapsed lease milliseconds |
| `LB_SUPERVISION_LOST` | Built-in `policy.supervision_required=true`; worker/watchdog evidence, or degraded supervision with observed exit 0 |
| `LB_REFUSED_MEMORY_BUDGET` | `policy.memory_budget_bytes`; requested admission reservation bytes |
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

Existing `requested_memory_bytes`/`granted_memory_bytes` remain hard-cap
fields. Separate `requested_memory_reservation_bytes` and
`granted_memory_reservation_bytes` describe admission. `memory_needed_bytes`
is the reservation needed by a queued request. An event with `correction=true`
repairs a previously published outcome for the same attempt. Use its latest
snapshot; it does not increment counters a second time. Historical counters
retain the originally observed classification.

Use `locked-build policy show` to inspect the effective policy. `locked-build policy init --force` regenerates it.

Tool manifest versions are minimums: an installed version at or above the pin passes, with SemVer prerelease ordering and build metadata ignored. Entries without `version` require only an executable on PATH; `check-install` never runs their help/version commands. Missing executables fail.

An explicit `CARGO_TARGET_DIR` is preserved by `locked-build run` and the Cargo shim, is excluded from per-lease trimming, and is cleaned by worktree hygiene.

## Install and verify a host

Drain both queued and running leases before upgrading this version. Old
binaries reject the new strict policy/lease fields. `install-tools` holds the
queue lock and refuses an active queue. The Make target finishes its admitted
Cargo install before running policy setup outside that lease. Installation
backs up the old combined memory policy, removes `memory_per_job_bytes`, adds
the 3 GB reservation and 8 GB hard-limit defaults, and preserves explicit new
values and hand-set jobs, threads, disk reserve and hold duration. A distinct
host policy wins over a competing fallback policy; the fallback is backed up.

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
