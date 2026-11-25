---
type: reference
---
# Host policy and telemetry

A host policy is a JSON document defining build admission and supervision limits.

## Fields

All fields except `sample_file` are required; unknown fields are rejected.
Relative filesystem paths resolve from the policy file's directory.
The polling interval cannot exceed the hold deadline or termination grace.

| Field | Meaning |
|---|---|
| `host` | Stable host label for requests and samples. |
| `capacity_units` | Currently enabled units; zero pauses admission. |
| `maximum_units` | Largest feasible request; temporary shrink preserves queue age. |
| `memory_per_unit_bytes` | RAM budget charged per requested unit. |
| `memory_reserve_bytes` | RAM retained outside build leases. |
| `disk_reserve_bytes` | Minimum free bytes for admission. |
| `disk_path` | Directory selecting the measured filesystem. |
| `maximum_load` | New admission requires load strictly below this value. |
| `maximum_priority` | Largest accepted base priority. |
| `aging_ms` | Positive priority aging interval. |
| `hold_ms` | Positive running command deadline. |
| `termination_grace_ms` | SIGTERM grace before forced cleanup. |
| `sample_max_age_ms` | Maximum host sample age. |
| `heartbeat_max_age_ms` | Age marking supervisor evidence suspect. |
| `poll_ms` | Positive supervisor observation cadence. |
| `maximum_requests` | Retained request limit; explicit pruning frees space. |
| `sample_file` | Host-agent sample path, or null for local measurements. |

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
