---
type: how-to
---
# Scrape build telemetry

To export build telemetry, start node_exporter with a textfile directory and run host installation after draining the build queue.
Use a persistent shared state directory and an installed locked-build. Read the [host policy reference](../reference.md) first.

1. From `tools/locked-build` on the host, check node_exporter's `--collector.textfile.directory` flag.
   Use an absolute directory writable by the account running locked-build.
2. Run `locked-build install-tools` outside an admitted lease after draining queued and running work.
   Installation detects the active node_exporter directory and renders the appropriate host schedules.
   Missing or conflicting directories stop installation with an error.
3. On Linux, inspect `systemctl --user status locked-build-metrics.timer locked-build-prune.timer`.
   On macOS, inspect `launchctl print gui/$(id -u)/com.agent-ix.locked-build-metrics`.
   Both platforms schedule metric refreshes and daily pruning of successfully exported requests.
4. Merge [prometheus.yml](prometheus.yml) into the server configuration and install [alerts.yml](alerts.yml) beside it.
   Preserve `job="node"` and each static `host` label. Import [dashboard.json](dashboard.json) into Grafana.
5. Run the monitoring checks from `tools/locked-build`:

   ```sh
   promtool check config monitoring/prometheus.yml
   promtool check rules monitoring/alerts.yml
   promtool test rules monitoring/alerts.test.yml
   locked-build metrics | promtool check metrics
   ```

## Expected output

The checks exit successfully. `ix_build_export_timestamp_seconds` advances and `ix_build_export_error` remains zero.
The dashboard has sixteen rows covering wait percentiles, starvation, resource use, scheduler health, fairness, failures, process ownership, and kache cache health.

Collection runs with low CPU and I/O priority. Collection has a six-second deadline and publication has an eight-second deadline. Linux also limits the service to ten seconds.
Set `IX_BUILD_KACHE` to the absolute kache executable path in the service environment if its `PATH` omits kache.
A failed refresh preserves previous measurements and marks the export error.
Back up state, policy, and the cumulative metrics ledger together. Restore their matching backup if the ledger is damaged.

See [metric semantics](metrics.md) for accounting, label bounds, retention, and histogram epochs.
