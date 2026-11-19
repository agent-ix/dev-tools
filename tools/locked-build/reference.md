---
type: reference
---
# Host policy and telemetry

A host policy is a JSON document defining build admission and supervision limits.

## Fields

All fields except `sample_file` are required; unknown fields are rejected.

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
Durations use a monotonic clock; UTC timestamps support cross-source correlation.

Requests move from `queued` to `running`, then `succeeded`, `failed`, `timed_out`, or `cancelled`.
A queued request can become `cancelled` without running.
Lost supervisors leave running requests suspect while their group remains live or unverified.
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

A running command's nonzero result is preserved instead of becoming a scheduler error.
