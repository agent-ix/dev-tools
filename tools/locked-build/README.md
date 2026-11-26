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
  run --owner draco:team:code --ticket TEAM-12 --units 1 --class point \
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
locked-build --state /tmp/build-queue --config /path/to/host-policy.json metrics
locked-build --state /tmp/build-queue --config /path/to/host-policy.json metrics \
  --output "$HOME/.local/share/node_exporter/textfile/ix_build_metrics.prom"
```

`--output` has no default; choose the collector path explicitly. The example uses
the per-user node_exporter directory. Terra WSL uses the shared file path
`/mnt/c/Users/<user>/.ix/metrics/ix_build_metrics.prom` instead.

`status` emits JSON with host capacity, occupied/free units, overcommit, host samples,
request wait/hold ages, uncertainty, outcomes, and stable transition events.
It omits command arguments and environment values.
Save terminal evidence before `prune`, which removes terminal requests, events, and output files.
`metrics` writes Prometheus text to stdout or atomically replaces the selected textfile.
Outcome counters, queue-wait histograms, and admission-blocked time are persisted in a
named sidecar and survive restart and `prune`; gauges describe the current queue,
occupied units, and configured capacity. Admission-blocked time is counted once per
host, even when several requests are waiting. No pause metric is exported.

For read-only host observations, run the separate sampler once per timer interval:

```sh
locked-build sample --config /etc/locked-build/sampler.json \
  --output "$HOME/.local/share/node_exporter/textfile/ix_build_metrics.prom"
```

The sampler does not use scheduler state or admission locks, and it does not inspect
build lock paths by opening or statting them. It reads only the process and source
paths declared in its strict sampler configuration, then atomically writes the
selected output file. It emits busy slots, configured capacity only where sourced,
and bytes under explicitly configured target directories. Unsupported queue and
outcome metrics are omitted. An unreadable source omits its affected metric and emits
a bounded `ix_build_probe_unavailable` signal. The sampler returns success after
writing available observations; invalid configuration or output-write failures return
an error.

## Scheduling

Higher effective priority runs first; equal priority follows enqueue order.
Effective priority adds one point per configured aging interval.
After `(maximum_priority + 1) * aging_ms`, the oldest feasible request reserves the next capacity.
Freed units accumulate for that request, including when it needs several units.
An occupied host cannot make a feasible large request ineligible for reservation.

When configured, lease policy sets `CARGO_BUILD_JOBS`, `RUST_TEST_THREADS`, and
`CARGO_INCREMENTAL` for the command. Cargo jobs are pressure-scaled as
`max(1, floor(configured_cargo_build_jobs / active_leased_units))`; active leased
units include the command being started. See [the policy reference](reference.md)
for optional fields and examples.

Use `--class must-fix`, `--class point`, `--class block`, or `--class periodic`
to use the configured named priorities. Numeric `--priority` remains available;
the two options are mutually exclusive.

When this executable is reached through a PATH entry named `cargo`, it acts as
a shim and queues the configured `real_cargo` binary. Set
`LOCKED_BUILD_CLASS`, `LOCKED_BUILD_OWNER`, and `LOCKED_BUILD_TICKET` to label a
request. Defaults are `point`, `cargo-shim`, and `MANUAL`; config and state
default to `$HOME/.config/locked-build/policy.json` and
`$HOME/.local/state/locked-build`. Leased commands carry a recursion marker so
nested Cargo calls execute the real binary directly.

With `target_root`, each owner gets a private `CARGO_TARGET_DIR` beneath that
root. On lease end, the tool deletes that owner's target when `trim_after_ms` or
`max_bytes` is exceeded. It skips cleanup while another lease for the owner is
active and refuses symlink entries in the target tree.

The default `live_available` memory policy uses measured available memory and
the existing per-unit reserve. `lease_accounted` instead admits a request only
when its memory cap plus active lease caps fit `memory_budget_bytes`; it never
reads live available memory. Per-class caps override the default cap. Linux
applies caps through a transient systemd scope with swap disabled, while the
macOS watchdog enforces the same cap from process-group RSS. A cap kill is
reported under the existing `rss_cap` outcome.

Capacity shrink preserves running commands and reports overcommit.
New admission waits until current capacity and resource reserves permit it.
In `live_available` mode, memory admission conservatively charges all declared
running budgets against measured available RAM. This can leave additional
headroom when running builds already consume their budgets.

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
