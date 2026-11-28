---
name: ix-build
description: Run or inspect shared-host build leases with ix-build. Use when coordinating Cargo builds, checking queued work, or diagnosing memory-cap outcomes.
---

# ix-build

The temporary `locked-build` symlink remains during the transition. See the
[namespace and error-code migration](../../tools/ix-build/README.md#transition-from-locked-build).

Use the installed `ix-build` command to submit and inspect builds. Do not write a private wrapper around it.

## Run a command

```sh
ix-build run -- cargo test -p package-name
```

Every lane of a user shares `/tmp/ix-build-<uid>` (owned, mode `0700`), regardless of lane `IX_HOME`. `~/.ix/ix-build/state` aliases that queue. Policy is a durable editable file at `~/.ix/ix-build/policy.json`; the legacy `~/.ix/locked-build/policy.json` remains supported. Installation preserves existing operator edits and recovers old temporary-policy aliases into durable files. Edit JSON directly: each admission pass rereads and validates it, queued workers use new values, and running leases retain their grants. `policy set KEY VALUE` optionally validates and atomically writes one change with a backup and before/after output. Explicit state/config overrides remain available for isolated fixtures. `status` reports `state_path`; `policy show` prints effective paths to stderr. Nested Cargo calls pass directly to real Cargo without state access when `IX_BUILD_LEASE=1`, or when the legacy runner sets a nonempty `CARGO_TARGET_DIR` and equal positive `CARGO_BUILD_JOBS`/`RUST_TEST_THREADS`.

New policy init ranks eligible writable local disks by device class: NVMe,
other SSD, HDD. Equal classes prefer HOME; secondary disks need100GB free.
Known read-only, removable, network and temporary mounts are excluded. Targets
use `<mount>/ix-build-<uid>/targets`, with `~/.ix/ix-build/targets` as
the HOME fallback; `disk_path` measures that selected filesystem. Existing
policies keep their hand-set paths. Jobs get `TMPDIR=<effective-target>/.tmp`,
including explicit caller targets; relative target paths resolve from command
cwd. A `.tmp` symlink is refused. Sandboxed lanes should select a writable
worktree target on the build disk when the shared host target is read-only.

Choose `--class must-fix`, `--class point`, `--class block`, or `--class periodic`. The default is `periodic`. Direct runs and the Cargo shim derive the owner from the user, host, and current worktree directory; `IX_BUILD_OWNER` overrides it. Ticket labels derive from the Git branch; `IX_BUILD_TICKET` overrides them. This gives each worktree its own Cargo target directory.

## Read request status

```sh
ix-build status
ix-build cancel REQUEST-ID
ix-build wait REQUEST-ID
```

`queued` means the request is accepted and waiting for admission. Its command has not started. `running` means it owns a lease and its command has started. Use the request id from status to cancel queued or running work.

One elected scheduler batches admissions every 250 ms and recovery every two
seconds. Queued workers and callers read notices, with a snapshot check at
least every two seconds. Status reads an atomic snapshot without taking the
state write lock. `wait` reattaches to an accepted request without enqueueing.

For a live Linux queue upgrade with an unchanged policy schema, stop only legacy
queued `run` callers. Fence queued workers using `recover --handoff-queued` in
their owning PID namespace. Reattach those request ids using `wait`.
Request ids and queue age survive. Keep running commands and their callers
alive. Other hosts require a drained legacy queue. Policy migrations retain
the installer's drain requirement. See the [queue reference](../../tools/ix-build/reference.md#queue-coordination).

Nested Cargo inherits `IX_BUILD_REAL_CARGO` from the admitted outer runner.
Its success or failure passes directly to that command without state access,
additional supervision, or signalling the outer lease. Linux memory sampling
rechecks children that disappear during a procfs read. Zombies contribute zero;
unreadable live processes retain the supervision-failure cleanup rule.

The generated `hold_ms` default is 5,400,000 ms (90 minutes), the ceiling for
any command; tune it in each host's policy. If the limit ends a command,
`ix-build status` and the `run` output name the limit and direct you to
change policy `hold_ms`.

Generated policies reserve `memory_reserve_per_job_bytes=3000000000` (3 GB)
per effective worker for admission, and enforce
`memory_limit_per_job_bytes=8000000000` (8 GB) per worker as a hard cap.
`memory_reserve_per_job_by_class` overrides reservations;
`memory_per_job_by_class` overrides hard limits. The sum of admitted
reservations must fit `memory_budget_bytes` (measured host RAM minus the host
reserve). Hard caps can sum above that budget while each command stays capped.
Generated class reservations use 1.25 GB/job for `block` and `must-fix`,
and 0.75 GB/job for `point`. Four jobs reserve 5 GB or 3 GB respectively;
hard limits keep the 8 GB/job fallback. Existing policies keep their values.

`rss_cap` means memory enforcement killed the command for exceeding its cap, including a cgroup scope whose `memory.events` reports `oom_kill`. Status includes `memory_cap_bytes`; `run` reports the request id, `rss_cap`, and cap in bytes. The observed child signal is retained as diagnostic data. `memory_cap_unavailable` means policy required memory enforcement that the launcher could not establish, so the command was not launched.

`supervision_lost` means the runner lost its watchdog, scope, or lease supervision. It terminates the whole command group with TERM and then SIGKILL after `termination_grace_ms` before returning. Linux recovery uses the kernel boot UUID so a drifting boot timestamp cannot expire a live lease.

`supervisor_lost` is recovery's label when its worker is absent/different and both owned groups are verified gone, without a recorded command outcome. PIDs are namespace-local. `install-tools` pins the host PID namespace and boot UUID once per boot. Host-side `recover` reclaims an extinct foreign namespace after checking its unique namespace init. Linux kills the namespace's members when that init exits. Readable `NSpid` fields distinguish host tasks and non-init members even when their namespace links require elevated permissions. An unreadable possible init retains the reservation with reason `foreign_pid_namespace_live_or_unverifiable`. Ordinary unprivileged host recovery needs no additional permission for host tasks' namespace links. Extinct running leases emit `IB_SUPERVISION_LOST` in their terminal event. Live namespaces retain their leases.

Use `ix-build recover --dry-run` to inspect foreign leases without writing state, policy, aliases or failure logs. For an uncertain lease, preview an explicit operator decision with `ix-build recover --dry-run --release <request-id> --reason "owner confirmed ended sandbox"`. Omit `--dry-run` only when authorized to release it. Release refuses observed live worker, watchdog or command evidence. It requires an expired hold or a provably dead owner worker. An accepted release sends no signals and records the operator UID, reason and prior stop in an `operator_release` event with `IB_SUPERVISION_LOST`.

The fallback watchdog counts only the launched command and its descendants, validated against the command's birth identity. Linux thread tasks are excluded so each address space is counted once. Linux uses summed `Pss_Anon` bytes from `/proc/<pid>/smaps_rollup`, proportionally accounting shared anonymous pages and excluding file/tmpfs mappings; macOS uses RSS. Each lease logs one initial sample with contributing PIDs/bytes; the sample that crosses the cap is saved in `lease_record.memory_sample`. A cap diagnostic includes measured bytes, cap bytes, and metric. Cgroup OOM diagnostics use `memory.peak` when available.

Each refusal or abnormal completion prints one diagnostic line to the caller
and saves that same text as `message` in the event ledger and status JSON:

```text
ix-build: <CODE> request=<id> limit=<policy key>=<value> measured=<value> (<what>) killed=<pid/command>
```

| Code | Limit source and measurement |
|---|---|
| `IB_RSS_CAP` | `policy.memory_limit_per_job_bytes*parallelism` (or the class override); measured command-tree `pss_anon` bytes on Linux, RSS on macOS |
| `IB_OOM_KILLED` | The same memory cap; the owned scope's `memory.events` counter or retained systemd `Result=oom-kill`. Reports sampled RSS and the actual counter, or counter unavailable with the Result source. `killed` names the owned scope when recorded. |
| `IB_HOLD_TIMEOUT` | `policy.hold_ms`; elapsed lease milliseconds |
| `IB_SUPERVISION_LOST` | Built-in `policy.supervision_required=true`; worker/watchdog evidence, or degraded supervision with observed exit 0 |
| `IB_REFUSED_MEMORY_BUDGET` | `policy.memory_budget_bytes`; requested admission reservation bytes |
| `IB_REFUSED_CLASS_CAP` | `policy.memory_cap_by_class.<class>`; requested lease bytes |
| `IB_REFUSED_QUEUE` | `policy.MAX_REQUESTS` or `policy.maximum_units`; retained requests or requested units |
| `IB_REFUSED_CAP_UNAVAILABLE` | `policy.on_missing_cgroup`; unavailable cap backend |
| `IB_CANCELLED` | Built-in `policy.run_permitted=true`; caller revoked permission |
| `IB_EXTERNAL_SIGNAL` | Built-in `policy.external_signals=none`; “terminated by signal N from outside ix-build” |
| `IB_COMMAND_EXIT` | Built-in `policy.success_exit_code=0`; actual command exit status |
| `IB_INVALID_REQUEST` | Built-in `policy.valid_request=true` or configured priority bound; rejected input |
| `IB_STATE_UNAVAILABLE` | Built-in `policy.state_access=read_write`; unavailable state/configuration |

Built-in rule names describe required behavior; they are not configurable
policy keys. An unavailable historical measurement is explicit. `killed`
identifies the process group or `not-started/command` for a refusal.
`ix-build error-codes` lists the enum. `code`, `limit_key`, `limit_value`,
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

Use `ix-build policy show` to inspect the effective policy. `ix-build policy init --force` regenerates it.

Tool manifest versions are minimums: an installed version at or above the pin passes, with SemVer prerelease ordering and build metadata ignored. Entries without `version` require only an executable on PATH; `check-install` never runs their help/version commands. Missing executables fail.

An explicit `CARGO_TARGET_DIR` is preserved by `ix-build run` and the Cargo shim, is excluded from per-lease trimming, and is cleaned by worktree hygiene.

## Install and verify a host

Drain both queued and running leases before upgrading this version. Old
binaries reject the new strict policy/lease fields. `install-tools` holds the
queue lock and refuses an active queue. The Make target finishes its admitted
Cargo build before running installation outside that lease. Installation
backs up the old combined memory policy, removes `memory_per_job_bytes`, adds
the 3 GB reservation and 8 GB hard-limit defaults, and preserves explicit new
values and hand-set jobs, threads, disk reserve and hold duration. A distinct
host policy wins over a competing fallback policy; the fallback is backed up.

From the dev-tools repository root, install the binary, measured policy, and
Cargo shim with `make install-tools ROLLOUT_OWNER=<stable-rollout-owner>`. The command puts the
shim ahead of real Cargo in the supported shell startup files. Start a new
shell after the first install. npm packages are installed with both
`--registry=https://registry.npmjs.org/` and
`--@agent-ix:registry=https://registry.npmjs.org/`, so a user or scope registry
setting cannot route Agent IX package installs through a different registry.
Run `ix-build check-install` to compare installed CLI pins against
`tools/ix-build/tool-versions.json`; its one-line `ok`/`mismatch` output
and exit status are suitable for ix-board cadence checks. This host manifest
does not pin Cargo or rustc; each repository owns its toolchain selection,
using `rust-toolchain` where configured. A `rust-version` value sets an MSRV
floor, not a host-wide toolchain pin.

Only the rollout owner pinned in `~/.ix/ix-build/install.json` may install.
Concurrent installs are rejected. The record stores source commit, artifact
SHA256, UTC install milliseconds and ownership-transfer history. Use
`ix-build install-tools --owner <new> --transfer-from <recorded-owner>` for
an explicit transfer. `check-install` prints these values and fails if a direct
binary write changed the installed artifact. Git builds embed HEAD; source
archives need `IX_BUILD_COMMIT=<full-sha> IX_BUILD_DIRTY=false` at build time
after verifying that the source archive is clean.

`max_queued_per_owner` is a positive policy override. The default is
`ceil(maximum_units/4)`, minimum one. Over-cap submissions get `IB_REFUSED_QUEUE`
with `limit=policy.max_queued_per_owner`, the attempted count, and the setting
to change. Only queued requests count. Status exports `queued_by_owner`
(owner-to-count map) and effective `max_queued_per_owner` (integer).

An abandoned launch with a proven-absent original client and no command members
ends `IB_SUPERVISION_LOST` within one watchdog poll, freeing its lease. Client
birth identities and PID namespaces are recorded separately from replaceable
workers. Live clients, live command members, foreign namespaces and uncertain
kernel group liveness retain capacity; a healthy command's exit remains authoritative.

`run --memory 16GiB` selects a total reservation and hard cap for that request,
without changing parallelism or class defaults. Sizes accept integer bytes or
KB/MB/GB and KiB/MiB/GiB. In `lease_accounted` mode the positive policy
`max_request_memory` ceiling defaults to half `memory_budget_bytes`;
oversized requests get `IB_REFUSED_CLASS_CAP` naming that key. Absolute
`memory_cap_by_class` and the host budget remain authoritative. Status/events
show requested/granted hard memory and reservation bytes; an explicit cap's
diagnostic limit source is `request.memory_bytes`. The status metrics and
Prometheus gauges `ix_build_active_memory_reserved_bytes` / `ix_build_active_memory_limit_bytes`
sum active sizes by `{host,class,state}`, where state is queued or running.

## Wrapper migration map

| Old command or state | New command |
|---|---|
| `ix-build.sh` build lane | `ix-build run --class <must-fix\|point\|block\|periodic> --owner <owner> --ticket <ticket> -- cargo ...` |
| `ix-build.sh` Kani lane | `ix-build run --class kani --owner <owner> --ticket <ticket> -- cargo kani ...` |
| `lead-reqs-cargo.sh` | `ix-build run --class must-fix --owner lead-reqs --ticket <ticket> -- cargo ...` |
| `arch-test-run.sh` | `ix-build worktree-run --repository <repo> --slot <n> --ticket <ticket> -- cargo test ...` |
| cargo-resource-controller jobs registry | `ix-build status` and `ix-build metrics` |
| per-lane `gate.sh` or raw `timeout ... cargo` | `ix-build run --class <class> --owner <owner> --ticket <ticket> -- cargo ...` |

The checked-in host capacity examples record core and RAM measurements for
draco (18 cores), luna (8), titan (32), and terra (14). Installation uses the
host's measured cores, RAM, and disk when
writing its local policy. Terra's Linux verification is run through `ssh cave`.
Windows is unsupported: the process supervisor uses Unix process and signal
APIs.
