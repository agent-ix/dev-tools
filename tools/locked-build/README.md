---
type: readme
---
# locked-build

locked-build queues shared-host builds and starts each command when its resource lease is admitted.

Refusals and abnormal completions report stable `LB_*` codes with the request,
policy limit, measurement and affected process in one stderr line. The same
diagnostic appears in status and the event ledger. Queue traces include rank,
resource blockers, per-state timing and requested/granted jobs and memory;
Prometheus counters and histograms summarize these by host and class. See the
[operator skill](../../skills/locked-build/SKILL.md) for the code catalog and
metric labels.

Memory-blocked heads permit fitting backfill until their bounded aging
reservation. Must-fix requests precede aged lower classes; admission never
overcommits the memory budget. Queued status includes required memory and free
budget. Linux reboot checks use the kernel boot UUID, macOS uses
`kern.boottime`, and timestamp-only fallback ignores jitter up to 120 seconds.

## Install all host admission tools

From the repository root, install locked-build, the Cargo shim, and a measured
host policy with one command:

```sh
make install-tools
```

The installer builds and copies the binary only when its bytes change. It
creates `~/.ix/locked-build/policy.json` from the host's core, memory, and
disk measurements, writes `~/.local/share/dev-tools/installed.json`, and adds
`~/.local/lib/locked-build/shims` ahead of the real Cargo path in shell startup
files. Re-running the command leaves identical files untouched. Start a new
shell after the first install.

All lanes use `/tmp/locked-build-<uid>` and its `policy.json`, independent of
lane `IX_HOME`. `~/.ix/locked-build/state` and `policy.json` are host aliases.
New policies select writable local build storage by device class: NVMe before
other SSDs, then HDDs. Equal classes prefer HOME; another disk needs at least
100 GB free. Read-only, removable and known network/temporary mounts are
excluded. Targets use `<mount>/locked-build-<uid>/targets`, or
`~/.ix/locked-build/targets` when HOME is the selected fallback. Existing host
policies retain their paths. Installation moves an existing policy from
`~/.config/locked-build/policy.json`, preserving host settings. Conflicting
legacy and host policies stop installation and leave both files intact.

Writable and read-only lanes use that same queue, owned by the user with mode
`0700`. If IX home is read-only, the runner uses the queue directly without
creating the symlink. A conflicting existing state path stops the command.
Existing canonical policy needs only read access. An absent policy is published
atomically once in the shared directory. Unwritable generated
target roots fall back to its `targets` directory. Explicit state and policy
paths remain authoritative. `status` reports `state_path`; `policy show` prints
the state and policy paths to stderr alongside policy JSON on stdout.

Each leased job with an effective target gets `TMPDIR=<target>/.tmp` on the
same target path. Explicit relative or absolute `CARGO_TARGET_DIR` remains
authoritative; relative paths resolve from the command's working directory.
A `.tmp` symlink is refused. For a sandbox that cannot write the host's shared
target, select a writable target inside its worktree on the build disk.

Inside an admitted run, the Cargo shim executes real Cargo directly, without
state access or policy creation. It recognizes `LOCKED_BUILD_LEASE=1` and the
legacy runner's nonempty `CARGO_TARGET_DIR` plus equal, positive
`CARGO_BUILD_JOBS` and `RUST_TEST_THREADS`. The outer runner retains admission.

Check pinned versions of the installed tools with:

```sh
locked-build check-install
```

The install target runs the version check after setup. Its machine-readable
one-line results report `ok` or `mismatch` per tool and it exits 0 when every
minimum is met and every presence-only executable is available, 1 otherwise.
Presence-only tools are checked without invoking their help/version commands.
For example, ix-board 0.12.3 meets a 0.12.2 minimum. Pins live in
[`tool-versions.json`](tool-versions.json); the host capacity examples in
[`host-capacity-examples.json`](host-capacity-examples.json) record measured
cores and RAM for draco (18), luna (8), titan (32), and terra (14). The
installed policy is generated from each host's own measurements.

The generated `hold_ms` policy default is 5,400,000 ms (90 minutes), the
ceiling for any command. Tune `hold_ms` in the host policy for each host.

## Run a command

```sh
locked-build run -- cargo test -p x
```

No owner, ticket, state, or config flags are required. Owner and ticket values
come from the environment or the current user, host, directory, and Git branch.
Use `--class must-fix`, `--class point`, `--class block`, or `--class periodic`;
the default class is `periodic`. Set `LOCKED_BUILD_OWNER` or
`LOCKED_BUILD_TICKET` to override the derived labels.

## Replace the retired build wrappers

| Retired mechanism | Replacement |
|---|---|
| `locked-build.sh` ordinary lanes | `locked-build run --class <must-fix\|point\|block\|periodic> --owner <owner> --ticket <ticket> -- cargo ...` |
| `locked-build.sh` Kani lane | `locked-build run --class kani --owner <owner> --ticket <ticket> -- cargo kani ...` |
| `lead-reqs-cargo.sh` | `locked-build run --class must-fix --owner lead-reqs --ticket <ticket> -- cargo ...` |
| `arch-test-run.sh` | `locked-build worktree-run --repository <repo> --slot <n> --ticket <ticket> -- cargo test ...` |
| `cargo-resource-controller` registry | `locked-build status` and `locked-build metrics` read the durable lease state |
| per-lane `gate.sh` and raw `timeout ... cargo` | `locked-build run --class <class> --owner <owner> --ticket <ticket> -- cargo ...` |

`worktree-run` serializes each repository slot, assigns it a stable owner, and
admits the command with the `arch-test` class. Kani uses a separate `kani`
class. Both classes are present in a generated host policy and metrics labels.
Build and test commands run on Linux and macOS. Windows is unsupported because
the process supervisor and signal handling use Unix APIs.

## Check a request

```sh
locked-build status
locked-build cancel REQUEST-ID
locked-build wait REQUEST-ID
```

`queued` means the request is accepted and waiting for capacity. Its command has
not started. `running` means the request holds a lease and its command has
started. Status includes the request id needed by `cancel`.

One elected scheduler handles admission and recovery for the store. Queued
workers read request notices without polling the state write lock. Status reads
an atomic snapshot and remains available while a writer holds that lock.
`wait` attaches to an existing request and returns its recorded exit status.
It preserves the request id and queue age.

In the default `lease_accounted` mode, Cargo leases reserve the resolved
parallelism times `memory_reserve_per_job_bytes` (default 3 GB) against the
host budget. Their enforced hard cap uses `memory_limit_per_job_bytes`
(default 8 GB) independently. Non-Cargo commands use requested units.
Reservations can be overridden per class with `memory_reserve_per_job_by_class`;
`memory_per_job_by_class` overrides hard limits. A `-j2` command reserves 6 GB
and can peak at 8.3 GB under its 16 GB cap. Admission never exceeds the sum of
reserved memory allowed by the measured budget.

Drain queued and running leases before this upgrade. Old binaries reject the
new strict fields; `install-tools` refuses an active queue and preserves a
backup of the old combined policy when migrating to separate reservation and
hard-limit settings. See the [operator skill](../../skills/locked-build/SKILL.md)
for migration and PID-namespace recovery details.

If memory enforcement kills a command for exceeding its cap, status reports
`rss_cap`, including a scope whose `memory.events` reports `oom_kill`.
Status includes `memory_cap_bytes`; `run` prints the request id, reason, and
cap in bytes. The direct child's signal remains available as diagnostic data.
If watchdog or scope supervision disappears, the runner terminates the whole
command group before returning and records `supervision_lost`. It sends TERM
and escalates to SIGKILL after the configured grace period. Linux recovery uses
the kernel boot UUID to distinguish a reboot from boot-timestamp drift.
Host-side recovery drains provably extinct PID namespaces and retains live or unverifiable leases.
Use `recover --dry-run` to preview decisions and `recover --release <request-id> --reason <text>` for an authorized, audited operator release.
The fallback watchdog counts only the command tree:
Linux proportional anonymous memory (`Pss_Anon`) or macOS RSS. Its initial
sample logs each contributing PID and bytes; cap diagnostics name the metric,
measured bytes, and cap.
A required systemd scope that cannot start with
`on_missing_cgroup=refuse` reports `memory_cap_unavailable` and does not launch
the command. When `hold_ms` ends a command, status and `run` output report the
configured limit and identify policy `hold_ms` as the setting to change.
See [reference.md](reference.md) for policy fields, telemetry, recovery, and
host setup.

## Export build telemetry

[Scrape build telemetry](monitoring/README.md) installs a per-host textfile refresh,
Prometheus alert rules, and a Grafana dashboard. [Metric semantics](monitoring/metrics.md)
defines duration, memory, capacity, failure, identity, and host-policy series.
