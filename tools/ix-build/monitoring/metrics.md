---
type: reference
---
# Build metric semantics

Build telemetry is a Prometheus view of the durable transition ledger and current scheduler state.
Use its counters to compare completed work and its gauges to inspect current queue health.

Admission holds add three host-only gauges: `ix_build_queue_paused` is 1 in Held
and 0 in Open; `ix_build_queue_hold_age_seconds` is the persisted hold's UTC age;
`ix_build_queue_hold_alert_after_seconds` is its configured alert bound. The age
and bound are zero in Open. LockedBuildQueueHoldOverdue fires after the age
exceeds the bound for one minute. Hold IDs and notes are never metric labels.

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

- `ix_build_active_memory_reserved_bytes{class,state}`: summed reservations for queued or running requests.
- `ix_build_active_memory_limit_bytes{class,state}`: summed hard limits for queued or running requests.

These histograms have `class` labels:

- `ix_build_memory_reserved_bytes`: completed lease reservations.
- `ix_build_memory_peak_bytes`: observed command peaks.

Finite boundaries are 100 million, 1 billion, 8 billion, and 32 billion bytes.
Cgroup peaks and sampled watchdog peaks share the peak histogram. Watchdog measurements are lower bounds between polls.
`ix_build_running_memory_headroom_ratio` is the minimum measured running headroom, computed as `1 - peak / cap`.
The headroom gauge is absent when no running lease has both measurements.

### Outcomes and attribution

- `ix_build_builds_total{class,outcome}` counts terminal attempts. Outcomes are `ok`, `failed`, `cancelled`, `timed_out`, and `refused`.
- `ix_build_failures_total{class,code}` counts abnormal attempts by stable `IB_*` diagnostic code. Memory kills count as failed builds.
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
Process observations are read-only. Their gauges remain absent when the process table or a lease's PID namespace cannot be verified.

`ix_build_oldest_queued_seconds{class}` measures the oldest current queue age, including saved reboot age.
`ix_build_queue_depth_by_reason{class,reason}` counts queued requests by their last observed blockers.
Simultaneous blockers overlap; Their sum indicates queue presence.
An empty blocker list is `ready`. Priority reservation becomes `fifo_or_priority_reservation`.

`ix_build_admission_blocked_seconds_total{reason}` measures host blocked time once per resource interval.
Reasons are `memory`, `target_disk`, `host_load`, and `probe`. Scheduler writes checkpoint and close these intervals.
Scrapes project an open interval without changing persisted state.

### Host and exporter

- `ix_build_supervisor_restarts_total`: replacement workers after their first generation.
- `ix_build_recover_events_total`: registered replacements and completed orphan finalizations.
- `ix_build_boot_changes_total`: detected host boot changes.
- `ix_build_telemetry_dropped_total`: durable dropped-event counter, initially zero.
- `ix_build_info{version,commit,dirty,policy_hash}`: compiled provenance and effective policy fingerprint, with value one.
- `ix_build_policy_hold_seconds`: maximum lease duration.
- `ix_build_policy_maximum_load`: maximum admission load average.
- `ix_build_export_timestamp_seconds`: last successful refresh time in UTC seconds.
- `ix_build_state_dir_free_bytes`: available bytes on the state directory filesystem.
- `ix_build_snapshot_size_bytes`: atomic snapshot file size.
- `ix_build_export_error`: one when collection failed or exceeded its six-second collection deadline; otherwise zero.

The policy fingerprint is FNV-1a over effective serialized policy. Compare hashes with each host's intended configuration.
Different hardware policies can produce intentional cross-host skew.

## Collection and retention

Collection reads atomic snapshots without taking the scheduler write lock. Separate collector and publisher processes bound probes and all fallback/publication I/O.
On failure, publication retains previous samples and their original refresh timestamp, then sets `ix_build_export_error` to one.
Atomic publication skips unchanged contents. Successful textfile publication records the exported store identity and ledger sequence.
The publisher has an eight-second hard deadline; a blocked output leaves the previous atomic file intact.
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

## Kache cache observations

Kache observations measure host cache activity and capacity. Both `sample` and the scheduled `metrics` exporter discover `kache` on `PATH`.
Set `IX_BUILD_KACHE` to an absolute executable path when the scheduled service has a restricted path.
The scheduled service needs the same `HOME` and configuration location as its kache daemon.
The probe preserves only home, XDG location variables, `KACHE_CONFIG`, and `KACHE_DIR` in its child environment.

Collection runs `config list --json`, `stats --json --since 1000000h`, and `explain --all --json` directly.
The three subprocesses run sequentially and each output is limited to one MiB.
In scheduled metrics, their shared deadline is five seconds after metrics collection begins; earlier scheduler observations consume that budget.
The collector has a six-second bound, and the publisher group has an eight-second bound covering publication and fallback I/O.
Probe children inherit the publisher group. Standalone `sample` gives the probe five seconds after its other host observations.
An absent executable emits no kache series. A failed probe emits `ix_kache_up=0` and `ix_kache_scrape_error=1` while other telemetry continues.
When configuration is unreadable, those failure series omit `store` until its classification is known.

Successful observations use `host` and `store` labels. Physical inventory stays labelled `store=local`; remote observations use `store=shared`.
`cache=store_0` through `store_3` identify the first four stores sorted by their reported paths; extra stores are counted by `store_inventory_truncated`.
The configured remote has `cache=remote` and a verified `backend` label.
The upstream [configuration reference](https://ninja.kunobi.com/docs/kache/getting-started/configuration) defines the nested key.
File inspection excludes environment overrides and daemon-effective configuration.
The label describes the host's cache mode; remote capacity is unavailable from the local stats report.
Raw paths, crate names, remote URLs, and error messages never become labels.
`ix_kache_info{version}` identifies the connected daemon version.
`ix_kache_daemon_expected{host}` defaults to zero until the ledger records a connected daemon.
A recorded connection keeps the expectation enabled across subsequent scrape failures.
Set `IX_BUILD_KACHE_EXPECT_DAEMON=1` to require a daemon explicitly, or `0` to disable that expectation.

These metric families describe the observed data:

| Family prefix `ix_kache_` | Type | Source |
| --- | --- | --- |
| `up`, `scrape_error` | Gauge | Daemon connection and probe outcome |
| `store_bytes`, `store_limit_bytes` | Gauge | Physical disk usage and configured cap |
| `entries` | Gauge | Sum of readable local store entries |
| `hits_total`, `misses_total`, `remote_hits_total` | Counter | Retained-history compile results |
| `hit_compile_seconds_total` | Counter | Compile time saved on hits |
| `miss_compile_seconds_total` | Counter | Miss elapsed time, including overhead |
| `hit_elapsed_seconds_total` | Counter | Hit elapsed time |
| `permit_wait_seconds_total` | Counter | Compiler permit wait |
| `gc_evicted_entries_total`, `gc_evicted_bytes_total` | Counter | Accumulated observed GC runs |
| `gc_last_run_timestamp` | Gauge | Last observed GC Unix timestamp |
| `counter_epoch` | Gauge | Unix timestamp of `lifetime.since` |
| `passthrough_total{reason}` | Counter | Retained-history reason counts |

The ledger `kache-metrics.json` lives beside scheduler state. `sample` uses a `.kache.json` sibling of its output file.
Each ledger has a nonblocking lock and atomic publication. A malformed ledger fails the probe and remains available for diagnosis.
Back up this ledger with scheduler state. Deleting it resets exporter counters.

The long stats window avoids treating the default sliding 24-hour view as a cumulative counter.
All compile counters use fields from the same retained event-history window, including `time_saved_ms` for hit compile savings.
`readonly_hits` is included in `local_hits`; the total adds only local, prefetch, and remote hits.
Counter differences accumulate in the ledger; a changed `lifetime.since` replaces the source baseline without adding retained counts again.
A decreasing source count starts a new baseline. Retained-history deletion can lose accuracy because the ledger counts only observed changes.
GC reports describe the latest run, as confirmed by the upstream [snapshot writer](https://github.com/kunobi-ninja/kache/blob/v2.4.0/src/report.rs#L202).
Each newer `last_run` contributes its per-run counts once, even across exporter restarts.
A null `last_run` contributes nothing and leaves scrape availability unchanged.
Multiple GC runs between observations cannot be recovered from that snapshot.
Passthrough reasons use three bounded buckets: `not_a_compile`, `unsupported`, and `other`.
A single local store renders 24 cache series, with one host daemon-expectation series added by the complete probe: 25 cache series.
Policy metadata adds one separate build-family series. All configurations stay within the tested budget of 60 cache series.

The dashboard adds cache hit ratio, store fill, hourly evictions, permit wait share, and scrape availability.
Hit-ratio and permit-wait alerts require at least ten observed compiler results in an hour.
Capacity, eviction, and scrape alerts require fifteen minutes of sustained failure.
Eviction alerts suppress windows containing exporter counter resets.
Scrape alerts require `daemon_expected=1`; an undiscovered executable emits no expectation series.

Kache reports an activity session identifier, but a session may span Cargo commands. Observed events expose roots and timestamps without a ix-build request identifier.
Per-build cache attribution requires an identifier shared with the admission request; host counters remain independent of build ownership.

## Shared-cache policy and observations

`cache.mode` accepts `local` or `shared`; `both` is a compatibility alias of shared. The default permits local reuse and sets `KACHE_LOCAL_ONLY=true` for each admitted command.
Shared mode explicitly permits the configured remote and retains local fallback; the wrapper rollout is separate.
`cache.cache_role` defaults to `client`; set `server` for a cache-serving host.
`ix_build_cache_policy_info{host,cache_role,cache_mode}` supports host joins for future disk/network panels.
No host is classified by its name.

Remote configuration fixtures cover the source-declared `cache.remote.type` values `s3`, `gcs`, `filesystem`, and `oci`.
These fixtures validate the installed release schema; no live remote backend was provisioned or contacted.
Local and remote hit counters are separate: `local_hits_total` includes local and prefetched restores, and `remote_hits_total` describes remote hits.
The existing `hits_total` remains the total cache-hit count for compatibility.

With shared policy enabled, an optional bounded full stats call supplies `remote_push_total`, `remote_pull_total`, transfer bytes, transfer errors, and download mean/p95 latency.
Full report output is capped at eight MiB and shares the collection deadline. Failure only clears `remote_observation_available`; local observations remain valid.
A recent full report file can instead be supplied through `cache.remote_report_file`.
`cache.remote_status_file` accepts a recent doctor JSON snapshot whose Remote check supplies `remote_reachable`: zero, one, or minus one for unknown.
Snapshots expire after 120 seconds. The exporter never runs doctor because its store probes can write temporary files.
A missing source remains unknown, never a successful remote health check.

The unreachable-remote fixture reports reachability zero, errors, and available local fallback while local stats and an admitted command remain successful.
`local_fallback` summarizes retained local hits and a recent failed remote check; it attributes no particular build.
Remote lookup misses, push latency, remote physical capacity, and cache-server client counts have no verified read-only JSON source in this release.
They are omitted. Live backend reachability, authentication, and real compiler fallback need the rollout pilot.

Per-store `store_inventory_bytes` uses `blob_stats.total_blob_size` for physical payload bytes; it is omitted when that source is unavailable.
The aggregate `store_bytes` remains the physical disk gauge.

`ix_build_info` includes compile-time `commit` and `dirty` labels alongside package version and policy fingerprint.
Unknown build provenance reports commit unknown with dirty=true; it never consults runtime Git state.
Standalone sampler `cache.remote_report_file` and `cache.remote_status_file` inputs must be absolute paths.
