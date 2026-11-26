---
type: readme
---
# locked-build

locked-build queues shared-host builds and starts each command when its resource lease is admitted.

## Install

From this directory, install with Rust and Make:

```sh
make install
```

The first command creates a host policy under `$XDG_CONFIG_HOME` or
`$HOME/.config`, and private state under `$XDG_STATE_HOME` or
`$HOME/.local/state`. Both directories are created on demand.

## Run a command

```sh
locked-build run -- cargo test -p x
```

No owner, ticket, state, or config flags are required. Owner and ticket values
come from the environment or the current user, host, directory, and Git branch.
Use `--class must-fix`, `--class point`, `--class block`, or `--class periodic`;
the default class is `periodic`. Set `LOCKED_BUILD_OWNER` or
`LOCKED_BUILD_TICKET` to override the derived labels.

## Check a request

```sh
locked-build status
locked-build cancel REQUEST-ID
```

`queued` means the request is accepted and waiting for capacity. Its command has
not started. `running` means the request holds a lease and its command has
started. Status includes the request id needed by `cancel`.

In the default `lease_accounted` mode, Cargo leases charge the resolved `-j`
count times 2.5 GB per job. Non-Cargo commands charge their requested units.
The scheduler admits a command only while the sum of running leases stays
within the measured memory budget.

If memory enforcement kills a command for exceeding its cap, status reports
`rss_cap`. A required systemd scope that cannot start with
`on_missing_cgroup=refuse` reports `memory_cap_unavailable` and does not launch
the command.
See [reference.md](reference.md) for policy fields, telemetry, recovery, and
host setup.
