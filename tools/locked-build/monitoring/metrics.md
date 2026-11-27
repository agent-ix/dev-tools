---
type: reference
---
# Build metric semantics

Build telemetry is a Prometheus view of the durable transition ledger and current scheduler state.
Status JSON exposes identical additional samples under `metrics.telemetry`, using the same shared records as the text exporter.

Every family has a `host` label. Histogram families have cumulative `_bucket`, `_sum`, and `_count` samples.
Durations are monotonic elapsed seconds; `wait` covers every terminal attempt, including queued cancellations and refusals.
`run` covers commands that started. `hold` includes startup after admission; its ratio uses the policy `hold_ms` cap.
New histogram groups appear after their first observation. Missing observations remain absent rather than becoming zero measurements.

| Families | Kind | Additional labels |
|---|---|---|
| `ix_build_wait_seconds`, `ix_build_run_seconds` | histogram | class, outcome |
| `ix_build_hold_seconds`, `ix_build_hold_limit_ratio` | histogram | class, outcome |
| `ix_build_memory_reserved_bytes` | histogram | class, outcome |
| `ix_build_memory_peak_bytes`, `ix_build_memory_headroom_ratio` | histogram | class, outcome, measurement |
| `ix_build_memory_kill_limit_bytes` | histogram | class, code |
| `ix_build_memory_kill_measured_bytes` | histogram | class, code, measurement |
| `ix_build_failures_total` | counter | class, code, exit_class, signal, cause |
| `ix_build_identity_total` | counter | class, outcome, lane, ticket, repo, subcommand |
| `ix_build_supervisor_restarts_total`, `ix_build_recover_events_total`, `ix_build_boot_changes_total` | counter | none |
| `ix_build_queue_depth_by_reason` | gauge | class, reason |
| `ix_build_owner_queued` | gauge | lane |
| `ix_build_running`, `ix_build_units_used`, `ix_build_units_total` | gauge | none |
| `ix_build_memory_budget_used_bytes`, `ix_build_memory_budget_total_bytes` | gauge | none |
| `ix_build_running_memory_headroom_ratio`, `ix_build_memory_measurements_available` | gauge | none |
| `ix_build_oldest_queued_seconds`, `ix_build_export_timestamp_seconds` | gauge | none |
| `ix_build_info` | gauge | version, policy_hash |
| `ix_build_policy_jobs`, `ix_build_policy_test_threads`, `ix_build_policy_maximum_units` | gauge | none |
| `ix_build_policy_hold_seconds`, `ix_build_policy_maximum_load` | gauge | none |
| `ix_build_policy_memory_reserve_bytes`, `ix_build_policy_disk_reserve_bytes` | gauge | none |
| `ix_build_policy_memory_reserve_per_job_bytes`, `ix_build_policy_memory_limit_per_job_bytes` | gauge | none |

The existing requests, terminations, blocked-seconds, queue-depth, lane-running, queue-wait, slots, builds, and admission-blocked families remain available.
`ix_build_blocked_seconds_total` includes active and completed per-request intervals. Simultaneous blockers overlap.
`queue_depth_by_reason` uses the same last observed blockers as status; summing its reasons can exceed total queue depth.
`priority_reservation` is exported as `fifo_or_priority_reservation`. An empty blocker list is `ready`, including requests awaiting their first scheduler probe.

Memory reservation is the admission charge; the hard cap controls enforcement.
`memory.peak` supplies cgroup peaks, while the watchdog retains the maximum observed RSS or Pss_Anon.
Watchdog peaks are lower bounds between polls. The `measurement` label distinguishes these accounting methods.
Headroom is `1 - peak / hard_cap`; a negative value records overshoot.
The running headroom gauge is the minimum across measured running leases, and is absent when no measurement exists.
`memory_measurements_available` gives that measured lease count. Avoid applying `rate` to headroom histogram sums, which can decrease after overshoots.
Live-available admission has no fixed budget: its budget-total gauge is zero. Optional unset jobs and threads also export zero.

Failure codes use the stable `LB_*` catalog, including all `LB_REFUSED_*` refusals.
`exit_class` is `101`, `other`, `zero`, or `none`. Signals are `1` through `64`, `other`, or `none`.
`cause=command` covers command exits and external signals; scheduler limits, refusals, cancellation, and supervision loss use `cause=tool`.
The existing termination counter emits zero series for every code, so kill alerts can detect the first failure after an initial scrape.

Identity labels retain at most 128 distinct lane/ticket/repo/subcommand tuples for the ledger's lifetime, plus one `other` tuple.
Classes and outcomes have closed vocabularies. The lane registry retains 64 addresses plus `other`; overflow stays aggregated after pruning.
Repositories resolve from the working directory's Git metadata, including linked worktrees.
Cargo subcommands are `build`, `test`, `check`, `clippy`, `xtask`, or `other`.
Request IDs, command arguments, environment values, and absolute paths are excluded from labels.

Supervisor restarts count replacement worker claims after the first generation.
Recover events count repaired requests that had a previous worker, or requests repaired after reboot; idle recovery polls add nothing.
Boot changes count detected changes between persisted host identities.
`policy_hash` fingerprints the effective serialized policy using FNV-1a; it identifies configuration changes and provides no cryptographic guarantee.
