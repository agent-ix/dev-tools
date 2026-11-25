---
type: readme
---
# locked-build

locked-build is a persistent build queue and process supervisor for shared Unix hosts.

Use it to admit builds within configured capacity, preserve FIFO ordering,
bound priority waiting, and inspect resource leases from several terminals.

## Getting started

Install Rust and Make on macOS or Linux. From this directory, run:

```sh
make install
```

Copy [policy.example.json](policy.example.json) to a host-owned policy file.
Set the host label, enabled capacity, maximum capacity, memory budget, load limit,
and disk reserve from that host's resource plan.
The example starts with zero enabled capacity, so it queues work until configured.
Every client on one host uses the same private state directory and policy path.

## Usage

This command runs a build under the host policy and returns its exit status:

```sh
locked-build --state /tmp/build-queue --config /path/to/host-policy.json \
  run --owner draco:team:code --ticket TEAM-12 --units 1 --priority 0 \
  -- cargo test -p selected-package
```

The command prints its request ID and output directory to stderr.
Command output goes to private `<request-id>.stdout` and `<request-id>.stderr` files there.
Exit 75 remains exit 75 and runs once.
A signal produces a nonzero exit; timeout returns 124 and cancellation returns 130.

For a wrapper that already admits work, `observe` records occupancy and
outcomes without making an admission decision:

```sh
locked-build --state /tmp/build-observations --config /path/to/host-policy.json \
  observe --kind build --priority check --queued-at-ms 1795120000000 \
  -- cargo test -p selected-package
```

The command starts immediately and inherits the caller's standard streams,
environment, working directory, and process group. `--kind` and `--priority`
are attribution hints only. Observed and admitted data use separate store
modes; admission commands refuse an observed store, while observe fails open
when given an admitted store.

Inspect, cancel, recover, or prune requests using the same state and policy options:

```sh
locked-build --state /tmp/build-queue --config /path/to/host-policy.json status
locked-build --state /tmp/build-queue --config /path/to/host-policy.json cancel REQUEST-ID
locked-build --state /tmp/build-queue --config /path/to/host-policy.json recover
locked-build --state /tmp/build-queue --config /path/to/host-policy.json prune
locked-build --state /tmp/build-queue --config /path/to/host-policy.json metrics
locked-build --state /tmp/build-queue --config /path/to/host-policy.json metrics \
  --output "$HOME/.local/share/node_exporter/textfile/ix_build_metrics.prom"
```

`--output` has no default; choose the collector path explicitly. The example uses
the per-user node_exporter directory. Terra WSL uses the shared file path
`/mnt/c/Users/<user>/.ix/metrics/ix_build_metrics.prom` instead.

`status` emits JSON with host capacity, occupied/free units, overcommit, host samples,
request wait/hold ages, uncertainty, outcomes, and stable transition events for
admitted stores. Observed stores expose only observed occupancy, configured
slots, and attributed outcomes or queue wait; they omit unknown scheduler values.
Both modes omit command arguments and environment values.
Save terminal evidence before `prune`, which removes terminal requests, events, and output files.
`metrics` writes Prometheus text to stdout or atomically replaces the selected textfile.
Outcome counters, queue-wait histograms, and admission-blocked time are persisted in a
named sidecar and survive restart and `prune`; gauges describe the current queue,
occupied units, and configured capacity. Admission-blocked time is counted once per
host, even when several requests are waiting. No pause metric is exported.
An observed store exports only `ok` and `failed` outcomes after the matching
kind and priority have started, queue-wait series only for observed priorities,
and no queue-depth, admission-blocked, pause, or paused-orphan series.

## Scheduling

Higher effective priority runs first; equal priority follows enqueue order.
Effective priority adds one point per configured aging interval.
After `(maximum_priority + 1) * aging_ms`, the oldest feasible request reserves the next capacity.
Freed units accumulate for that request, including when it needs several units.
An occupied host cannot make a feasible large request ineligible for reservation.

Capacity shrink preserves running commands and reports overcommit.
New admission waits until current capacity and resource reserves permit it.
Memory admission conservatively charges all declared running budgets against measured available RAM.
This can leave additional headroom when running builds already consume their budgets.

## Supervision and recovery

Each request has a primary supervisor and a watchdog that owns its command process group.
The watchdog starts before the build and enforces the hold deadline, SIGTERM grace, and SIGKILL cleanup.
A lease remains occupied until the kernel confirms that the command group has disappeared.
Process enumeration alone cannot release capacity.
The watchdog temporarily leaves its group to check for exit; its live PID prevents group-ID reuse during that check.
Command creation and exit checks share the state lock.
Commands must keep their descendants in that group; detached services require their own service manager.

A supervisor survives the submitting terminal's exit.
`recover` restarts queued supervisors and checks orphaned running groups.
Restarted queued supervisors inherit the recovering invocation's environment.
Supply the same build environment when recovering queued work.
Running commands are never rerun after supervisor loss.
The watchdog continues cancellation and deadline enforcement if the primary supervisor dies.
The lease remains occupied until command exit is verified.
Process identity requires the supervisor token, birth time, and group identity; PID existence alone cannot authorize signaling.
Generation fencing prevents a superseded worker from admitting or completing another worker's lease.

State and command arguments are private files; keep credentials in inherited environment or external secret providers.
Replace a policy or host sample atomically so readers see a complete document.
Use [the reference](reference.md) for field and telemetry definitions.

## Migration

An external wrapper may use `observe` after it admits a build. Keep that wrapper
as the admission authority; `observe` records metrics and does not schedule or
terminate work.
Operator agent-count annotations describe planning capacity.
Read measured build leases from the shared scheduler.

## Building and testing

```sh
make fmt-check
make lint
make test
```

## License

MIT, under the repository [license](../../LICENSE).
