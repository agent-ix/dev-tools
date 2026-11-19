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

Inspect, cancel, recover, or prune requests using the same state and policy options:

```sh
locked-build --state /tmp/build-queue --config /path/to/host-policy.json status
locked-build --state /tmp/build-queue --config /path/to/host-policy.json cancel REQUEST-ID
locked-build --state /tmp/build-queue --config /path/to/host-policy.json recover
locked-build --state /tmp/build-queue --config /path/to/host-policy.json prune
```

`status` emits JSON with host capacity, occupied/free units, overcommit, host samples,
request wait/hold ages, uncertainty, outcomes, and stable transition events.
It omits command arguments and environment values.
Save terminal evidence before `prune`, which removes terminal requests, events, and output files.

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

Each request has an isolated supervisor process group.
A companion watchdog joins that group before the build starts.
The supervisor and watchdog enforce the hold deadline, SIGTERM grace, and SIGKILL cleanup.
A lease remains occupied until its command group has stopped.
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

Point new build invocations at the shared state directory and host policy.
Route every cooperating wrapper through this executable before treating its capacity view as complete.
Let existing builds finish under their original wrappers before retiring those wrappers.
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
