---
type: reference
---
# Build metric semantics

Build telemetry is a Prometheus view of the durable transition ledger and current scheduler state.
Use its counters to compare completed work and its gauges to inspect current queue health.

## Fields

Every family has a `host` label. Priority classes are `must-fix`, `point`, `block`, `periodic`, `kani`, and `arch-test`.
Unavailable measurements remain absent. Histogram groups appear after their first observation.
All histogram buckets are cumulative and include `+Inf`, `_sum`, and `_count`.

### Durations

These histograms have `class` labels:

- `ix_build_wait_seconds`: queue duration of terminal attempts, including cancellations and refusals.
- `ix_build_run_seconds`: execution duration of commands that started.
- `ix_build_hold_seconds`: admitted lease duration, including startup.

Finite boundaries are 1, 10, 60, 300, 900, 3600, and 7200 seconds.
Queue age includes time saved at reboot recovery. Cross-boot run and hold durations remain absent.

### Memory

These histograms have `class` labels:

- `ix_build_memory_reserved_bytes`: completed lease reservations.
- `ix_build_memory_peak_bytes`: observed command peaks.

Finite boundaries are 100 million, 1 billion, 8 billion, and 32 billion bytes.
Cgroup peaks and sampled watchdog peaks share the peak histogram. Watchdog measurements are lower bounds between polls.
`ix_build_running_memory_headroom_ratio` is the minimum measured running headroom, computed as `1 - peak / cap`.
The headroom gauge is absent when no running lease has both measurements.

### Outcomes and attribution

- `ix_build_builds_total{class,outcome}` counts terminal attempts. Outcomes are `ok`, `failed`, `cancelled`, `timed_out`, and `refused`.
- `ix_build_failures_total{class,code}` counts abnormal attempts by stable `LB_*` diagnostic code. Memory kills count as failed builds.
- `ix_build_identity_total{repo,subcommand,outcome}` counts terminal attempts. Attribution outcomes are `ok`, `failed`, and `other`.
- `ix_build_repo_run_seconds_total{repo,subcommand}` accumulates observed execution seconds.
- `ix_build_repo_run_count_total{repo,subcommand}` counts corresponding measured runs.
- `ix_build_wasted_run_seconds_total{class,cause}` accumulates measured execution seconds lost to cancellation or limit kills. Causes are `cancelled` and `late_kill`.
- `ix_build_lane_unit_seconds_total{lane}` accumulates completed hold seconds multiplied by admitted units.

Repositories resolve through Git metadata, including linked worktrees. The repository registry retains 30 names plus `other` for its lifetime.
Subcommands are `build`, `test`, `check`, `clippy`, `xtask`, and `other`.
The lane registry retains 64 three-part addresses plus `other` for its lifetime.
The identity counter aggregates independently of lane and ticket. Repository attribution uses counters throughout.

### Capacity and queue health

These gauges have only the `host` label:

- `ix_build_running`: admitted leases, including startup.
- `ix_build_units_used`: occupied admission units.
- `ix_build_units_capacity`: configured admission capacity.
- `ix_build_memory_budget_used_bytes`: current reservations.
- `ix_build_memory_budget_total_bytes`: configured memory budget; zero means live-available mode.
- `ix_build_oldest_running_seconds`: oldest admitted lease age.
- `ix_build_leases_missing_process`: running leases with neither a live worker nor command identity.
- `ix_build_unleased_build_processes`: live Cargo, rustc, or rustc-wrapper processes outside verified lease ancestry.

Process sampling verifies PID and start time before accepting a lease root. Zombies are excluded.
Process observations are read-only. Their gauges remain absent when the process table cannot be sampled.

`ix_build_oldest_queued_seconds{class}` measures the oldest current queue age, including saved reboot age.
`ix_build_queue_depth_by_reason{class,reason}` counts queued requests by their last observed blockers.
Simultaneous blockers overlap; their sum indicates queue presence rather than exact queue size.
An empty blocker list is `ready`. Priority reservation becomes `fifo_or_priority_reservation`.

`ix_build_admission_blocked_seconds_total{reason}` measures host blocked time once per resource interval.
Reasons are `memory`, `target_disk`, `host_load`, and `probe`. Scheduler writes checkpoint and close these intervals.
Scrapes project an open interval without changing persisted state.

### Host and exporter

- `ix_build_supervisor_restarts_total`: replacement workers after their first generation.
- `ix_build_recover_events_total`: registered replacements and completed orphan finalizations.
- `ix_build_boot_changes_total`: detected host boot changes.
- `ix_build_telemetry_dropped_total`: durable dropped-event counter, initially zero.
- `ix_build_info{version,policy_hash}`: exporter version and effective policy fingerprint, with value one.
- `ix_build_policy_hold_seconds`: maximum lease duration.
- `ix_build_policy_maximum_load`: maximum admission load average.
- `ix_build_export_timestamp_seconds`: last successful refresh time in UTC seconds.
- `ix_build_state_dir_free_bytes`: available bytes on the state directory filesystem.
- `ix_build_snapshot_size_bytes`: atomic snapshot file size.
- `ix_build_export_error`: one when collection failed or exceeded its eight-second deadline; otherwise zero.

The policy fingerprint is FNV-1a over effective serialized policy. Compare hashes with each host's intended configuration.
Different hardware policies can produce intentional cross-host skew.

## Collection and retention

Collection reads atomic snapshots without taking the scheduler write lock. A separate collector process bounds store and process probes.
On failure, publication retains previous samples and their original refresh timestamp, then sets `ix_build_export_error` to one.
Atomic publication skips unchanged contents. Successful textfile publication records the exported store identity and ledger sequence.
Daily pruning removes terminal requests covered by that checkpoint and retains cumulative counters.

The exporter and its node scrape use the `node` job's static `host` label.
The textfile path comes from the running node_exporter's `--collector.textfile.directory` flag.
Installation requires one unambiguous absolute directory.

Stored lifetime counters migrate to the current labels. Changed histogram boundaries start a fresh histogram epoch.
Repository execution and lane unit-second counters begin accumulating from new transitions after upgrade.
Upgrade all writers together before resuming the queue.

## Examples

`ix_build:wait_p95_seconds{class="must-fix"}` reads the five-minute queue wait p95.
`ix_build:repo_run_mean_seconds` reads lifetime mean execution seconds per repository and subcommand.
`ix_build:lease_overhead_seconds_per_second` subtracts execution time from lease time per class.
The [dashboard](dashboard.json) groups the named recording queries into fifteen queue-health questions.
The [alert rules](alerts.yml) share those recordings.
