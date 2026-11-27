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

Generated policies use `memory_per_job_bytes=6000000000` (6 GB) per effective Cargo job or non-Cargo unit. Existing policies keep their configured estimates. A lease charges its parallelism times that estimate against the host budget.

`rss_cap` means memory enforcement killed the command for exceeding its cap, including a cgroup scope whose `memory.events` reports `oom_kill`. Status includes `memory_cap_bytes`; `run` reports the request id, `rss_cap`, and cap in bytes. The observed child signal is retained as diagnostic data. `memory_cap_unavailable` means policy required memory enforcement that the launcher could not establish, so the command was not launched.

`supervision_lost` means the runner lost its watchdog, scope, or lease supervision. It terminates the whole command group with TERM and then SIGKILL after `termination_grace_ms` before returning. Linux recovery uses the kernel boot UUID so a drifting boot timestamp cannot expire a live lease.

`supervisor_lost` is recovery's label when its worker is absent/different and both owned groups are verified gone, without a recorded command outcome. It is not an observed command exit code. PIDs are namespace-local: recovery in another PID namespace retains the lease and never infers death or respawns its worker from those numbers.

The fallback watchdog counts only the launched command and its descendants, validated against the command's birth identity. Linux thread tasks are excluded so each address space is counted once. Linux uses summed `Pss_Anon` bytes from `/proc/<pid>/smaps_rollup`, proportionally accounting shared anonymous pages and excluding file/tmpfs mappings; macOS uses RSS. Each lease logs one initial sample with contributing PIDs/bytes; the sample that crosses the cap is saved in `lease_record.memory_sample`. A cap diagnostic includes measured bytes, cap bytes, and metric. Cgroup OOM diagnostics use `memory.peak` when available.

Use `locked-build policy show` to inspect the effective policy. `locked-build policy init --force` regenerates it.

Tool manifest versions are minimums: an installed version at or above the pin passes, with SemVer prerelease ordering and build metadata ignored. Entries without `version` require only an executable on PATH; `check-install` never runs their help/version commands. Missing executables fail.

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
