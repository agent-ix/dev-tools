---
type: reference
---
# Host policy and telemetry

A host policy is a JSON document defining build admission and supervision limits.

## Fields

Every policy field has a serde default; unknown fields are rejected. A small
policy can set only `host`, `capacity_units`, and `maximum_units`. `lease_accounted`
requires a positive `memory_budget_bytes` and `memory_per_job_bytes`. It charges
each request by its resolved Cargo job count times the per-job estimate;
non-Cargo commands use requested units. `memory_per_unit_bytes` and
`memory_reserve_bytes` are required only in `live_available` mode.
Relative filesystem paths resolve from the policy file's directory.
On first use, locked-build measures the host and writes a conservative policy.
Use `locked-build policy show` to print it or `locked-build policy init --force`
to regenerate it.

`make install-tools` builds and copies the locked-build executable only when it
changes, generates a measured policy on first install, installs the `cargo`
shim, and writes `~/.local/share/dev-tools/installed.json`. It places the shim
directory before `~/.local/bin` in `.profile`, `.bashrc`, and `.zshrc`.
`locked-build check-install` compares the checked-in
[`tool-versions.json`](tool-versions.json) pins with each executable. Entries
with `version` run `--version` and compare the reported version; entries
without `version` resolve the executable and run `--help`, passing only when it
exits successfully. It prints one `ok` or `mismatch` line per tool and exits 1
when any tool is missing, fails `--help`, or has a different pinned version.

| Field | Meaning |
|---|---|
| `host` | Stable host label for requests and samples. |
| `capacity_units` | Currently enabled units; zero pauses admission. |
| `maximum_units` | Largest feasible request; temporary shrink preserves queue age. |
| `memory_per_unit_bytes` | RAM budget charged per requested unit in `live_available` mode; optional in `lease_accounted` mode. |
| `memory_reserve_bytes` | RAM retained outside build leases in `live_available` mode; optional in `lease_accounted` mode. |
| `memory_admission` | `lease_accounted` (default) sums fixed leases against `memory_budget_bytes` without reading live available memory; `live_available` compares `MemAvailable` with lease-unit charges. |
| `memory_budget_bytes` | Total memory available to concurrent leases in `lease_accounted` mode. |
| `memory_per_job_bytes` | Default memory estimate per effective Cargo job or non-Cargo unit; generated policies use 2.5 GB. |
| `memory_per_job_by_class` | Optional per-class per-job estimates, bounded by the class cap when one is configured. |
| `memory_cap_by_class` | Optional maximum total lease size for each named class. |
| `on_missing_cgroup` | `rss_watchdog` (default) enforces a configured cap with process-group RSS when a systemd scope is unavailable; `refuse` fails the lease without running the command. |
| `disk_reserve_bytes` | Minimum free bytes for admission. |
| `disk_path` | Directory selecting the measured filesystem. |
| `maximum_load` | New admission requires load strictly below this value. |
| `aging_ms` | Positive priority aging interval. |
| `hold_ms` | Positive running command deadline. |
| `termination_grace_ms` | SIGTERM grace before forced cleanup. |
| `sample_file` | Host-agent sample path, or null for local measurements. |
| `cargo_build_jobs` | Optional maximum Cargo jobs at one active leased unit. At spawn, divide by the total active leased units and clamp to at least one. |
| `rust_test_threads` | Optional `RUST_TEST_THREADS` value set for each leased command. |
| `cargo_incremental` | Optional incremental compilation setting; true sets `CARGO_INCREMENTAL=1`, false sets it to `0`. |
| `target_root` | Optional root for owner-specific `CARGO_TARGET_DIR` directories, used only when the caller has not set `CARGO_TARGET_DIR`. Relative paths resolve from the policy file. |
| `max_bytes` | Optional size limit for generated per-owner targets; caller-supplied `CARGO_TARGET_DIR` paths are never trimmed by lease cleanup. |
| `priority_classes` | Mapping of `must-fix`, `point`, `block`, `periodic`, `arch-test`, and `kani` to numeric priorities. The largest value sets the accepted priority limit. |
| `real_cargo` | Optional Cargo executable path used when this binary is invoked with `argv[0]` basename `cargo`. Relative paths resolve from the policy file. |

Explicit command-line `-j`/`--jobs` and `--test-threads` values, and explicit
`CARGO_BUILD_JOBS` and `RUST_TEST_THREADS` environment values, take precedence
over policy defaults. Values above the configured policy maximum are clamped;
the request report records the effective values and clamp flags. Pressure
scaling applies only when the caller leaves `CARGO_BUILD_JOBS` unset. Active
leased units include the command being started. The formula is
`max(1, floor(configured_cargo_build_jobs / active_leased_units))`; for example,
a policy value of 8 with 3 active units sets `CARGO_BUILD_JOBS=2`.
When the caller has not set `CARGO_TARGET_DIR` and target lifecycle is enabled,
each owner receives one generated directory under `target_root`. After the
owner's final active lease, cleanup removes that directory when its measured
size exceeds `max_bytes`. A caller-supplied `CARGO_TARGET_DIR` takes precedence
and is never trimmed by per-lease cleanup; worktree hygiene owns cleanup of a
worktree's `target/`. Symlink entries are skipped and never followed. Reaching
the bounded walk's entry limit marks a generated target oversized and removes
the whole directory.
`run --class NAME` looks up `NAME` in `priority_classes`; omitting class records
class `periodic` and uses its configured priority, or zero when absent. Metrics
use a `class` label with `must-fix`, `point`, `block`, `periodic`, `arch-test`,
or `kani`.

`worktree-run --repository NAME --slot N --ticket T -- COMMAND` waits for the
exclusive state-file lock for that repository slot, then submits an arch-test
job as owner `arch-test:NAME:slot-N`, class `arch-test`, and one resource unit.
Different slot numbers run concurrently. The Kani replacement is
`run --class kani -- ...`.

Scheduler commands and the PATH shim read policy from `--config`, then
`LOCKED_BUILD_CONFIG`, then `$XDG_CONFIG_HOME/locked-build/policy.json` or
`$HOME/.config/locked-build/policy.json`. State resolves from `--state`, then
`LOCKED_BUILD_STATE`, then `$XDG_STATE_HOME/locked-build` or
`$HOME/.local/state/locked-build`; the state directory is created mode `0700`.
The persisted state file is `<state-directory>/state.json`.
Relative XDG values are ignored. The shim reads
`LOCKED_BUILD_CLASS`, `LOCKED_BUILD_OWNER`, and `LOCKED_BUILD_TICKET`, defaulting
to `point`, `cargo-shim`, and `MANUAL`. A leased command receives
`LOCKED_BUILD_LEASE=1`; when a shim invocation sees that marker, it execs
`real_cargo` directly to avoid queue recursion.

In `lease_accounted` mode, admission requires the sum of active leases plus
the new request to be at most `memory_budget_bytes`; requests larger than the
whole budget fail at enqueue. The systemd `MemoryMax` and RSS fallback cap equal
the computed lease size. A zero live `memory_available_bytes` value does not
block the request. Linux starts capped commands as
`systemd-run --user --scope --quiet -p MemoryMax=<cap>
-p MemorySwapMax=0 -- <command>`. On macOS, where systemd-run is unavailable,
the watchdog samples process-group RSS and kills a group that exceeds its cap.
Cap-triggered termination is counted as `rss_cap`.
The request report records whether the selected enforcement was `systemd_scope`,
`rss_watchdog`, or `refused`.

Resource observations use available RAM, available filesystem bytes, and one-minute load.
An external sample includes exactly these fields:

```json
{
  "host": "your-build-host",
  "observed_at_ms": 1795120000000,
  "memory_available_bytes": 4294967296,
  "disk_available_bytes": 85899345920,
  "load_one": 1.0
}
```

The host must match the policy; stale or future samples block admission.
Missing measurements remain unavailable.

## Read-only sampler

`sample` reads a separate strict JSON configuration and writes Prometheus text to the
required output path. It does not open scheduler state or inspect admission/build lock
paths. Source adapters may read process metadata, `/proc/<pid>/fdinfo`, and the
declared controller registry; target-directory measurements enumerate only the
explicitly configured directories. The sampler has no timer of its own; a host timer
invokes it at the desired interval.

The sampler configuration uses `schema_version`, `host`, optional `capacity_units`,
one typed `busy_source`, and optional `target_dirs`. Target entries have `repo` and
absolute `path` fields. For example, Terra's source configuration is:

```json
{
  "schema_version": 1,
  "host": "terra",
  "capacity_units": 12,
  "busy_source": {
    "kind": "terra_wrapper_fdinfo",
    "proc_root": "/proc",
    "owner_uid": 1000,
    "wrapper_path": "/home/<user>/dev/worktrees/locked-build.sh",
    "lane_paths": [
      "/tmp/quire-heavy-check.lane1.lock", "/tmp/quire-heavy-check.lane2.lock",
      "/tmp/quire-heavy-check.lane3.lock", "/tmp/quire-heavy-check.lane4.lock",
      "/tmp/quire-heavy-check.lane5.lock", "/tmp/quire-heavy-check.lane6.lock",
      "/tmp/quire-heavy-check.lane7.lock", "/tmp/quire-heavy-check.lane8.lock",
      "/tmp/quire-heavy-check.lane9.lock", "/tmp/quire-heavy-check.lane10.lock",
      "/tmp/quire-heavy-check.lane11.lock", "/tmp/quire-heavy-check.lane12.lock"
    ]
  },
  "target_dirs": [{"repo": "dev-tools", "path": "/home/build/dev-tools/target"}]
}
```

The `lane_paths` array must contain all 12 configured ordinary lane paths. Terra
filters process directories by `owner_uid`, reads `stat` to select `bash`, and then
matches only cmdlines whose `argv[1]` equals `wrapper_path`. It reads fdinfo only after
the corresponding fd symlink target matches a configured lane path. A matching lane
counts only when fdinfo contains a `FLOCK WRITE` record, so queued wrappers with an
open but unlocked lane descriptor do not count as busy. Host source types and
capacities are constrained as follows:

| Host | Busy source | Configured capacity |
|---|---|---:|
| Draco | Distinct configured slot paths whose live `/usr/bin/lockf -t 0` process has a child | 6 |
| Terra | Distinct configured lane paths with a `FLOCK WRITE` record held by the configured wrapper process | 12 |
| Titan | Live cargo-resource-controller jobs, FLOCK lanes held by the two configured build wrappers, and arch-test FLOCK lanes, deduplicated by process group | omitted |
| Luna | Top-level `cargo` process trees running build, test, or check commands | omitted |

Titan has three observed lock-backed sources in addition to the controller registry:
the 13 lanes in `/home/<user>/dev/worktrees/locked-build.sh` (12 ordinary lanes and
the separate Kani lane), three `cargo-slot.sh` lanes under the QSL handoff worktree,
and per-repository arch-test lanes held by `/home/<user>/arch-test-run.sh`. Configure
both build wrappers explicitly in `wrapper_sources`; the validator requires both
observed wrapper basenames and their sourced lane counts. `arch_lock_prefix` is the
absolute prefix ending in `arch-test-target-`, such as
`/home/<user>/dev/worktrees/arch-test-target-`. The arch adapter reads fdinfo only
for the configured `arch-test-run.sh` process and only when an fd symlink matches a
valid repository lane path. It never opens or stats lock paths. Wrapper adapters
similarly filter to the configured Bash script before inspecting descriptors. Active
process groups are unioned across the controller registry and all lock sources, so an
inherited descriptor or a build visible through multiple sources counts once.

Titan's mixed, partially uncapped controls do not establish a host-wide configured
slot count. Omit `capacity_units`; do not derive a total from the per-repository arch
lanes or from the wrapper ceilings.

The Titan source shape is:

```json
{
  "schema_version": 1,
  "host": "titan",
  "busy_source": {
    "kind": "titan_build_wrappers_and_arch_locks",
    "proc_root": "/proc",
    "owner_uid": 1000,
    "registry_path": "/home/<user>/.local/state/cargo-resource-controller/jobs.json",
    "arch_lock_prefix": "/home/<user>/dev/worktrees/arch-test-target-",
    "arch_wrapper_path": "/home/<user>/arch-test-run.sh",
    "wrapper_sources": [
      {
        "wrapper_path": "/home/<user>/dev/worktrees/locked-build.sh",
        "lane_paths": [
          "/tmp/quire-heavy-check.lane1.lock", "/tmp/quire-heavy-check.lane2.lock",
          "/tmp/quire-heavy-check.lane3.lock", "/tmp/quire-heavy-check.lane4.lock",
          "/tmp/quire-heavy-check.lane5.lock", "/tmp/quire-heavy-check.lane6.lock",
          "/tmp/quire-heavy-check.lane7.lock", "/tmp/quire-heavy-check.lane8.lock",
          "/tmp/quire-heavy-check.lane9.lock", "/tmp/quire-heavy-check.lane10.lock",
          "/tmp/quire-heavy-check.lane11.lock", "/tmp/quire-heavy-check.lane12.lock",
          "/tmp/quire-heavy-check.lane-kani.lock"
        ]
      },
      {
        "wrapper_path": "/home/<user>/dev/worktrees/QSL-handoff/<worktree>/cargo-slot.sh",
        "lane_paths": [
          "/home/<user>/dev/worktrees/QSL-handoff/<worktree>/cargo-slots/slot1",
          "/home/<user>/dev/worktrees/QSL-handoff/<worktree>/cargo-slots/slot2",
          "/home/<user>/dev/worktrees/QSL-handoff/<worktree>/cargo-slots/slot3"
        ]
      }
    ]
  }
}
```

Replace `<user>` and `<worktree>` with the host's actual account and QSL handoff
worktree. The validator rejects a config missing either observed wrapper or any
configured lane.

The sampler emits `ix_build_slots_busy` when its busy source can be read and emits
`ix_build_slots_configured` only for Draco and Terra. Queue depth and build/test outcome
series are omitted because these sources cannot truthfully count them. Each explicitly
configured target directory produces `ix_build_target_dir_bytes{host,repo}`. If a
source cannot be read, its affected metric is omitted; `ix_build_probe_unavailable`
identifies the bounded probe name and a bounded diagnostic is written to stderr. The
sampler writes metrics from successful sources and returns success after the output
write; invalid configuration or output-write failures return an error. Process,
fdinfo, and target-directory enumeration have hard bounds;
reaching a bound makes the affected measurement unavailable. Terra's waiter ticket
files are readable, but the current queue-depth metric requires a priority label and
those tickets do not provide one; the sampler omits queue depth rather than inventing
an attribution.

## Transition events

`status` and `recover` emit a versioned JSON snapshot.
Each transition has stable store/event/request IDs, host, owner, ticket,
UTC time, resulting state, priority, units, capacity/free units, wait/hold durations, and reason.
Consumers deduplicate by event ID and retain their own history before pruning.
`pruned_events` counts removed transitions; `next_event_sequence` exposes missing history.
Durations within one boot use a monotonic clock; UTC timestamps support cross-source
correlation. Queue-wait histograms across a reboot add the pre-reboot age captured at
recovery to the monotonic wait after recovery.

Requests move from `queued` to `running`, then `succeeded`, `failed`, `timed_out`, or `cancelled`.
A queued request can become `cancelled` without running.
After supervisor loss, the authenticated watchdog continues timeout and cancellation enforcement.
The request remains suspect and occupied until command exit is verified.
Release requires a kernel process-group absence result, including after a supervisor crash.
An incomplete or empty process enumeration preserves occupancy until that independent check succeeds.
No new grant exceeds configured capacity; reduced capacity can report existing overcommit.

## Examples

List stable error codes independently from a host policy:

```sh
locked-build error-codes
locked-build error-codes state
```

Read a selected host:

```sh
locked-build --state /tmp/build-queue --config /path/to/host-policy.json status
```

## Errors

The CLI returns 2 for invalid input, unavailable state, or policy errors.
Diagnostics begin with a stable code:

| Code | Meaning |
|---|---|
| `configuration` | Invalid host policy or a missing required CLI option. |
| `io` | Filesystem, process, or operating-system operation failed. |
| `state` | Invalid, unsupported, oversized, or unsafe persisted state. |
| `request` | Unknown request or invalid requested resource bounds. |

A running command's nonzero result is preserved as its own outcome.

## Prometheus metrics

`metrics` writes Prometheus text to standard output. `metrics --output PATH` atomically
replaces a textfile collector document; `--output` has no default. Use the per-user
node_exporter path `$HOME/.local/share/node_exporter/textfile/ix_build_metrics.prom`;
Terra WSL uses `/mnt/c/Users/<user>/.ix/metrics/ix_build_metrics.prom` instead. The exporter emits
`ix_build_queue_depth` and `ix_build_queue_wait_seconds` by the FR-011 classes
`check`, `test`, and `gate`;
configured priorities above 2 use the `gate` series,
`ix_build_slots_busy`, and `ix_build_slots_configured` from the host policy's
`capacity_units` field, which is the source basis for this configured capacity
series; it does not label this value as sustainable capacity. It exports
`ix_build_builds_total` for the closed FR-015 outcomes `ok`, `failed`, `cancelled`,
`timed_out`, `aborted_memory`, `aborted_swap`, `aborted_compressor`,
`aborted_host_load`, `aborted_thermal`, `rss_cap`, and `pause_exceeded`, plus
`ix_build_admission_blocked_seconds_total` for memory, target disk, host load, and
probe failures. A blocked interval is counted once per host and closes when the last
queued request leaves. This slice adds no pause or abort mechanism: abort outcome
names remain in the closed outcome set but never increment here; FR-015-AC-8 coverage
is limited to non-abort outcomes. The pause-specific metric series is omitted entirely;
`pause_exceeded` is present only as a closed build outcome label. Named metrics data is
stored in a separate `metrics.json` sidecar so old scheduler state remains readable.
Event counters and histogram observations survive `recover`, process restart, and
`prune` without being counted twice. Queue-wait histograms add the pre-reboot age
captured at recovery to monotonic time after recovery. The hold deadline remains
enforced by the existing watchdog behavior.
