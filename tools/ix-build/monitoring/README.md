---
type: how-to
---
# Scrape build telemetry

To export build telemetry, start node_exporter with a textfile directory and run host installation after draining the build queue.
Use a persistent shared state directory and an installed ix-build. Read the [host policy reference](../reference.md) first.

1. From `tools/ix-build` on the host, check node_exporter's `--collector.textfile.directory` flag.
   Use an absolute directory writable by the account running ix-build.
2. Run `ix-build install-tools` outside an admitted lease after draining queued and running work.
   Installation detects the active node_exporter directory and renders the appropriate host schedules.
   Missing or conflicting directories stop installation with an error.
3. On Linux, inspect `systemctl --user status ix-build-metrics.timer ix-build-prune.timer`.
   On macOS, inspect `launchctl print gui/$(id -u)/com.agent-ix.ix-build-metrics`.
   Both platforms schedule metric refreshes and daily pruning of successfully exported requests.
4. Merge [prometheus.yml](prometheus.yml) into the server configuration and install [alerts.yml](alerts.yml) beside it.
   Preserve `job="node"` and each static `host` label. Import [dashboard.json](dashboard.json) into Grafana.
5. Run the monitoring checks from `tools/ix-build`:

   ```sh
   promtool check config monitoring/prometheus.yml
   promtool check rules monitoring/alerts.yml
   promtool test rules monitoring/alerts.test.yml monitoring/dashboard.test.yml
   ix-build metrics | promtool check metrics
   ```

## Expected output

The checks exit successfully. `ix_build_export_timestamp_seconds` advances and `ix_build_export_error` remains zero.
The Build storage and cache dashboard has three rows: kache health, builds and queue health, and storage.
Select the build filesystem mountpoint and device before interpreting the storage panels.
Use cache_role to inspect cache servers or build clients.
Build-time trends describe daily performance; one-off marketing comparisons are separate data mining.

Collection runs with low CPU and I/O priority. Collection has a six-second deadline and publication has an eight-second deadline. Linux also limits the service to ten seconds.
Set `IX_BUILD_KACHE` to the absolute kache executable path in the service environment if its `PATH` omits kache.
A failed refresh preserves previous measurements and marks the export error.
Back up state, policy, and the cumulative metrics ledger together. Restore their matching backup if the ledger is damaged.

See [metric semantics](metrics.md) for accounting, label bounds, retention, and histogram epochs.

## Confirm a rollout

After installing a reviewed release, confirm the target host appears in Prometheus with these queries:

```promql
up{job="node",host="luna"}
time() - ix_build_export_timestamp_seconds{host="luna"}
ix_build_export_error{host="luna"}
ix_build_cache_policy_info{host="luna"}
ix_kache_up{host="luna"}
ix_kache_info{host="luna"}
sum(increase(ix_build_repo_run_count_total{host="luna"}[15m]))
sum(increase(ix_kache_hits_total{host="luna"}[15m]) + increase(ix_kache_misses_total{host="luna"}[15m]))
```

Replace luna with the rolled-out host. Node up should be one, export error zero, and the refresh age should remain near the collection interval.
Build and cache activity counters advance when compiler work occurs. A Cargo invocation with an already populated target may perform no compiler work.
Inspect the request's wrapper decision when confirming activation; missing traffic alone does not prove the wrapper was skipped.
A local-only host has no remote reachability series. A configured remote without recent reachability evidence reports minus one, meaning unknown.
