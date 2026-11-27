---
type: reference
---
# Build metric semantics

Build telemetry is a Prometheus view of the durable transition ledger and current scheduler state.
Status JSON exposes identical additional samples under `metrics.telemetry`, using the same shared records as the text exporter.

Every family has a `host` label. Histogram families have cumulative `_bucket`, `_sum`, and `_count` samples.
New histogram groups appear after their first observation. Unavailable measurements remain absent.

## Duration histograms

These families add `class` and `outcome` labels:

- `ix_build_wait_seconds`: queue duration for terminal attempts, including queued cancellations and refusals.
- `ix_build_run_seconds`: execution duration for commands that started.
- `ix_build_hold_seconds`: admitted lease duration, including command startup.
- `ix_build_hold_limit_ratio`: lease duration divided by its policy `hold_ms` cap.

Durations use monotonic clocks and retain saved queue age through reboot and admission.
Run and hold observations are unavailable for commands interrupted by reboot, because monotonic timestamps cross different boots.

## Memory histograms

Admission reservations charge the memory budget; hard caps control enforcement.
These completed-lease families add `class` and `outcome` labels:

- `ix_build_memory_reserved_bytes`: admission reservation.
- `ix_build_memory_peak_bytes`: observed peak, also labeled by `measurement`.
- `ix_build_memory_headroom_ratio`: `1 - peak / hard_cap`, also labeled by `measurement`.

These kill families add `class` and `code` labels:

- `ix_build_memory_kill_limit_bytes`: hard cap on OOM or RSS termination.
- `ix_build_memory_kill_measured_bytes`: measured peak, also labeled by `measurement`.

`memory.peak` supplies cgroup peaks. The watchdog retains the maximum observed RSS or Pss_Anon.
Watchdog peaks are lower bounds between polls. The `measurement` label distinguishes the accounting methods.
Negative headroom records overshoot; avoid `rate` on headroom histogram sums, which can decrease.

## Capacity gauges

These families add the specified labels:

- `ix_build_queue_depth_by_reason`: queued depth by `class` and `reason`; simultaneous blockers overlap.
- `ix_build_owner_queued`: queued depth by bounded `lane`.

These gauges have only the `host` label:

- `ix_build_running`: admitted leases, including command startup.
- `ix_build_units_used`: occupied admission units.
- `ix_build_units_capacity`: configured admission capacity.
- `ix_build_memory_budget_used_bytes`: current admission reservations.
- `ix_build_memory_budget_total_bytes`: configured fixed memory budget, zero in live-available mode.
- `ix_build_running_memory_headroom_ratio`: minimum measured running lease headroom, absent when unavailable.
- `ix_build_memory_measurements_available`: running leases with a measured peak and hard cap.
- `ix_build_oldest_queued_seconds`: oldest current queued age.
- `ix_build_export_timestamp_seconds`: UTC time of the latest refresh.

Queue reasons use the same last observed blockers as status. Their sum can exceed total queue depth.
`priority_reservation` is exported as `fifo_or_priority_reservation`.
An empty blocker list is `ready`, including requests awaiting their first scheduler probe.

## Failure and identity counters

`ix_build_failures_total` adds `class`, `code`, `exit_class`, `signal`, and `cause` labels.
Codes use the stable `LB_*` catalog, including all `LB_REFUSED_*` refusals.
`exit_class` is `101`, `other`, `zero`, or `none`. Signals are `1` through `64`, `other`, or `none`.
`cause=command` covers command exits and external signals.
Scheduler limits, refusals, cancellation, and supervision loss use `cause=tool`.

`ix_build_identity_total` adds `class`, `outcome`, `lane`, `ticket`, `repo`, and `subcommand` labels.
It retains 128 distinct lane/ticket/repo/subcommand tuples for the ledger's lifetime, plus one `other` tuple.
Classes and outcomes have closed vocabularies. The lane registry retains 64 addresses plus `other`; overflow stays aggregated after pruning.
Repositories resolve from working-directory Git metadata, including linked worktrees.
Cargo subcommands are `build`, `test`, `check`, `clippy`, `xtask`, or `other`.
Request IDs, command arguments, environment values, and absolute paths are excluded from labels.

## Host and policy

These counters have only the `host` label:

- `ix_build_supervisor_restarts_total`: replacement workers that registered after the first generation.
- `ix_build_recover_events_total`: registered replacements and completed orphan finalizations.
- `ix_build_boot_changes_total`: detected changes between persisted host identities.

Idle recovery polls and live pending first-launch claims add nothing.

`ix_build_info` is a gauge with `version` and `policy_hash` labels.
The hash fingerprints effective serialized policy with FNV-1a and identifies configuration changes.

These policy gauges have only the `host` label:

- `ix_build_policy_jobs`: configured maximum Cargo jobs; zero means unset.
- `ix_build_policy_test_threads`: configured test threads; zero means unset.
- `ix_build_policy_maximum_units`: largest feasible request size.
- `ix_build_policy_hold_seconds`: maximum lease duration.
- `ix_build_policy_maximum_load`: maximum admission load average.
- `ix_build_policy_memory_reserve_bytes`: host memory reserve.
- `ix_build_policy_disk_reserve_bytes`: host disk reserve.
- `ix_build_policy_memory_reserve_per_job_bytes`: default per-job admission charge.
- `ix_build_policy_memory_limit_per_job_bytes`: default per-job hard cap.

These policy gauges also add `class`:

- `ix_build_policy_class_memory_reserve_per_job_bytes`: effective per-class charge.
- `ix_build_policy_class_memory_limit_per_job_bytes`: effective per-class hard cap.
- `ix_build_policy_class_memory_cap_bytes`: maximum class lease size; zero means unset.

## Existing families

The existing families remain available:

- `ix_build_requests_total`: completed and refused attempts by class and outcome.
- `ix_build_terminations_total`: abnormal outcomes by class and code, including initial zero series.
- `ix_build_blocked_seconds_total`: active and completed per-request blocked time by class and reason.
- `ix_build_queue_depth`: queued requests by class.
- `ix_build_lane_running`: running leases by bounded lane.
- `ix_build_queue_wait_seconds`: queue duration at admission by class.
- `ix_build_slots_busy`, `ix_build_slots_configured`: occupied and configured units.
- `ix_build_builds_total`: completed builds by class and outcome.
- `ix_build_admission_blocked_seconds_total`: host blocked time by resource reason.

Simultaneous per-request blockers overlap. Initial zero termination series let kill alerts detect the first failure after an initial scrape.
