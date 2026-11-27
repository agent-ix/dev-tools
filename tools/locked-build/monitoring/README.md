---
type: how-to
---
# Scrape build telemetry

Export each host's build ledger to node_exporter, then load the dashboard and alerts into Prometheus and Grafana.
Use an installed locked-build with telemetry support and a working node_exporter. Read the [host policy reference](../reference.md) first.
The commands below run as `peter` from `tools/locked-build` on each host during the post-merge rollout.

## Persist the queue

`metrics.json` is the cumulative event ledger. Each transition is folded exactly once before pruning.
Keep it with `state.json` and `policy.json` on persistent storage, and back up the directory together.
The default `/tmp/locked-build-<uid>` location can be cleared by the operating system.

Drain the host queue and stop its callers before migrating the existing directory:

```sh
mkdir -p "$HOME/.local/state"
cp -a /tmp/locked-build-$(id -u) "$HOME/.local/state/locked-build"
chmod 700 "$HOME/.local/state/locked-build"
```

The destination must be absent before copying. Retain the original until verification finishes.
Set these variables in every lane's launch environment and login shell, then resume callers:

```sh
export LOCKED_BUILD_STATE="$HOME/.local/state/locked-build"
export LOCKED_BUILD_CONFIG="$LOCKED_BUILD_STATE/policy.json"
locked-build policy show
locked-build status
```

Compare the store ID and existing counters before and after migration. All lanes must use the same directory.
Process restarts, reboot recovery, and pruning preserve its ledger.
Deleting the directory creates a new store and a new counter baseline.
A corrupt ledger or missing event sequence stops export and leaves the last textfile intact.
Restore the matching backup; avoid deleting a damaged ledger to resume counters at zero.
Older executables cannot preserve new telemetry fields when writing the ledger; drain and upgrade every writer together.

## Refresh the textfile

Create the collector directory and verify one export:

```sh
mkdir -p "$HOME/.local/share/node_exporter/textfile_collector"
locked-build metrics --output "$HOME/.local/share/node_exporter/textfile_collector/locked-build.prom"
```

Start node_exporter with `--collector.textfile.directory` pointing to that directory.
The exporter needs read access to the directory and `.prom` file.
The metrics command serializes concurrent exports and atomically replaces a complete file.

On Luna, Terra's Linux environment, and Titan, install the user timer:

```sh
mkdir -p "$HOME/.config/systemd/user"
cp monitoring/locked-build-metrics.service monitoring/locked-build-metrics.timer "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
systemctl --user enable --now locked-build-metrics.timer
systemctl --user start locked-build-metrics.service
systemctl --user status locked-build-metrics.timer
```

Enable user lingering if the timer must survive logout. Terra's collector runs inside its Linux environment;
configure port forwarding so Prometheus can reach that environment's node_exporter.

On Draco, install the launch agent, which contains absolute paths for Peter's account:

```sh
mkdir -p "$HOME/Library/LaunchAgents"
cp monitoring/com.agent-ix.locked-build-metrics.plist "$HOME/Library/LaunchAgents/"
launchctl bootstrap gui/$(id -u) "$HOME/Library/LaunchAgents/com.agent-ix.locked-build-metrics.plist"
```

Both examples refresh every 15 seconds. Check their logs after reboot and confirm `ix_build_export_timestamp_seconds` advances.

## Load monitoring

Merge [prometheus.yml](prometheus.yml) into the Prometheus configuration and install [alerts.yml](alerts.yml) alongside it.
Replace target DNS names or ports with reachable node_exporter endpoints; preserve each static `host` label.
A dedicated `locked-build` scrape job lets the missing-host alert identify a host even when its exporter is unavailable.

Import [dashboard.json](dashboard.json) in Grafana and select the Prometheus datasource.
The Host and Class selectors filter panels; timing panels show p50, p95 and p99.
Tune alert thresholds against observed build durations and host budgets.

```sh
promtool check config monitoring/prometheus.yml
promtool check rules monitoring/alerts.yml
promtool test rules monitoring/alerts.test.yml
locked-build metrics | promtool check metrics
```

See [metric semantics](metrics.md) for accounting, unavailable measurements, and label limits.
