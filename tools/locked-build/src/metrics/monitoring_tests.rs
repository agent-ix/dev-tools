// SPDX-License-Identifier: MIT
//! Production queries evaluated against exported, replayed transition fixtures.
use super::tests::{event, policy, request, state};
use super::*;
use crate::model::{MemoryMetric, MemorySample, Phase};
use locked_build::wire::Code;
use serde_json::{Value, json};
use std::process::{Command, Stdio};

fn replay_series() -> Result<Vec<Value>> {
    let mut metrics = MetricsState::default();
    let mut sequence = 0;
    let mut samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for minute in 0..=30 {
        for (phase, wait, run, hold, code) in [
            ("succeeded", 30_000, 120., 150_000, None),
            ("cancelled", 600_000, 60., 80_000, Some(Code::Cancelled)),
            ("failed", 1_200_000, 600., 660_000, Some(Code::RssCap)),
        ] {
            sequence += 1;
            let mut e = event(sequence, phase, 2, wait);
            e.owner = "draco:tooling:one".into();
            e.units = 2;
            e.hold_ms = Some(hold);
            e.trace.running_at_ms = Some(1);
            e.trace.running_seconds = run;
            e.trace.diagnostic.code = code;
            e.telemetry.repo = "dev-tools".into();
            e.telemetry.subcommand = "test".into();
            e.telemetry.reservation_bytes = Some(200);
            e.telemetry.memory_sample = Some(MemorySample {
                bytes: 100,
                metric: MemoryMetric::Rss,
                contributors: vec![],
            });
            metrics.reconcile_events(&[e], sequence + 1)?;
        }
        sequence += 1;
        let mut refused = event(sequence, "failed", 1, 0);
        refused.kind = "refused".into();
        refused.trace.diagnostic.code = Some(Code::RefusedMemoryBudget);
        metrics.reconcile_events(&[refused], sequence + 1)?;
        sequence += 1;
        let mut lost = event(sequence, "failed", 0, 0);
        lost.class = "kani".into();
        lost.trace.diagnostic.code = Some(Code::SupervisionLost);
        metrics.reconcile_events(&[lost], sequence + 1)?;
        sequence += 1;
        let mut restart = event(sequence, "queued", 0, 0);
        restart.kind = "supervisor_restart".into();
        metrics.reconcile_events(&[restart], sequence + 1)?;
        let mut queued = request(1, "draco:tooling:one".into(), Phase::Queued);
        queued.class = "point".into();
        queued.active_blocked_reasons = vec!["units".into(), "memory_budget".into()];
        let mut cfg = policy()?;
        cfg.capacity_units = 8;
        if minute > 10 {
            cfg.maximum_load = 10.;
        }
        let current = state(vec![queued]);
        let text = metrics.exposition(&current, &cfg, minute * 60_000, 1_200_000)?;
        let mut present = BTreeMap::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            if let Some((name, value)) = line.rsplit_once(' ') {
                present.insert(name.to_owned(), value.to_owned());
            }
        }
        // Read-only host observations use the same appended gauge interface as runtime.
        for (name, value) in [
            ("ix_build_leases_missing_process", 0.),
            ("ix_build_unleased_build_processes", 0.),
            ("node_load1", 15.),
        ] {
            let mut gauge = String::new();
            crate::telemetry::append_gauge(&mut gauge, name, "draco", value);
            for line in gauge.lines().filter(|l| !l.starts_with('#')) {
                if let Some((name, value)) = line.rsplit_once(' ') {
                    present.insert(name.to_owned(), value.to_owned());
                }
            }
        }
        present.insert("ix_build_units_used{host=\"luna\"}".into(), "4".into());
        present.insert("ix_build_units_capacity{host=\"luna\"}".into(), "8".into());
        for (name, values) in &mut samples {
            values.push(present.remove(name).unwrap_or_else(|| "stale".into()));
        }
        for (name, value) in present {
            let mut values = vec!["_".into(); usize::try_from(minute).unwrap_or(0)];
            values.push(value);
            samples.insert(name, values);
        }
    }
    Ok(samples
        .into_iter()
        .map(|(series, values)| json!({"series":series,"values":values.join(" ")}))
        .collect())
}

fn expected(_record: &str, labels: &str, value: f64) -> Value {
    json!({"labels":format!("{{{labels}}}"),"value":(value*1_000_000.).round()/1_000_000.})
}

/// Trace: FR-015-AC-1, FR-015-AC-3, FR-015-AC-10
#[test]
#[ignore = "external PromQL acceptance lane; IX_BUILD_PROMTOOL must name promtool"]
fn telemetry_every_dashboard_question_matches_hand_written_replay_values() -> Result<()> {
    let entries: Vec<(&str, Vec<Value>)> = vec![
        (
            "wait_p50_seconds",
            vec![
                expected("wait_p50_seconds", "class=\"point\",host=\"draco\"", 600.),
                expected("wait_p50_seconds", "class=\"block\",host=\"draco\"", 0.5),
                expected("wait_p50_seconds", "class=\"kani\",host=\"draco\"", 0.5),
            ],
        ),
        (
            "wait_p95_seconds",
            vec![
                expected("wait_p95_seconds", "class=\"point\",host=\"draco\"", 3195.),
                expected("wait_p95_seconds", "class=\"block\",host=\"draco\"", 0.95),
                expected("wait_p95_seconds", "class=\"kani\",host=\"draco\"", 0.95),
            ],
        ),
        (
            "wait_p99_seconds",
            vec![
                expected("wait_p99_seconds", "class=\"point\",host=\"draco\"", 3519.),
                expected("wait_p99_seconds", "class=\"block\",host=\"draco\"", 0.99),
                expected("wait_p99_seconds", "class=\"kani\",host=\"draco\"", 0.99),
            ],
        ),
        (
            "oldest_queued_seconds",
            PriorityClass::ALL
                .into_iter()
                .map(|c| {
                    expected(
                        "oldest_queued_seconds",
                        &format!("class=\"{}\",host=\"draco\"", c.label()),
                        if c == PriorityClass::Point { 1200. } else { 0. },
                    )
                })
                .collect(),
        ),
        (
            "idle_with_queue",
            vec![expected("idle_with_queue", "host=\"draco\"", 1.)],
        ),
        (
            "reserved_memory_bytes",
            vec![expected(
                "reserved_memory_bytes",
                "class=\"point\",host=\"draco\"",
                200.,
            )],
        ),
        (
            "peak_memory_bytes",
            vec![expected(
                "peak_memory_bytes",
                "class=\"point\",host=\"draco\"",
                100.,
            )],
        ),
        (
            "kills_per_second",
            vec![expected(
                "kills_per_second",
                "host=\"draco\",code=\"LB_RSS_CAP\"",
                0.016666666666666666,
            )],
        ),
        (
            "load_policy_ratio",
            vec![expected("load_policy_ratio", "host=\"draco\"", 1.5)],
        ),
        (
            "restarts_per_second",
            vec![expected(
                "restarts_per_second",
                "host=\"draco\"",
                0.016666666666666666,
            )],
        ),
        (
            "supervision_lost_per_second",
            vec![expected(
                "supervision_lost_per_second",
                "host=\"draco\"",
                0.016666666666666666,
            )],
        ),
        (
            "queue_stuck",
            vec![expected("queue_stuck", "host=\"draco\"", 1.)],
        ),
        (
            "lane_unit_seconds_per_second",
            vec![expected(
                "lane_unit_seconds_per_second",
                "host=\"draco\",lane=\"draco:tooling:one\"",
                29.666666666666668,
            )],
        ),
        (
            "cancelled_per_second",
            vec![expected(
                "cancelled_per_second",
                "host=\"draco\"",
                0.016666666666666666,
            )],
        ),
        (
            "late_kills_per_second",
            vec![expected(
                "late_kills_per_second",
                "host=\"draco\"",
                0.016666666666666666,
            )],
        ),
        (
            "wasted_run_seconds_per_second",
            vec![
                expected(
                    "wasted_run_seconds_per_second",
                    "cause=\"cancelled\",host=\"draco\"",
                    1.,
                ),
                expected(
                    "wasted_run_seconds_per_second",
                    "cause=\"late_kill\",host=\"draco\"",
                    10.,
                ),
            ],
        ),
        (
            "lease_overhead_seconds_per_second",
            vec![expected(
                "lease_overhead_seconds_per_second",
                "class=\"point\",host=\"draco\"",
                1.8333333333333333,
            )],
        ),
        (
            "refusals_per_second",
            vec![expected(
                "refusals_per_second",
                "host=\"draco\",code=\"LB_REFUSED_MEMORY_BUDGET\"",
                0.016666666666666666,
            )],
        ),
        (
            "policy_epochs",
            vec![expected("policy_epochs", "host=\"draco\"", 2.)],
        ),
        (
            "unit_utilization_ratio",
            vec![
                expected("unit_utilization_ratio", "host=\"draco\"", 0.),
                expected("unit_utilization_ratio", "host=\"luna\"", 0.5),
            ],
        ),
        (
            "leaked_leases",
            vec![expected("leaked_leases", "host=\"draco\"", 0.)],
        ),
        (
            "unleased_builds",
            vec![expected("unleased_builds", "host=\"draco\"", 0.)],
        ),
        (
            "repo_run_mean_seconds",
            vec![expected(
                "repo_run_mean_seconds",
                "host=\"draco\",repo=\"dev-tools\",subcommand=\"test\"",
                260.,
            )],
        ),
        (
            "memory_measurement_ratio",
            vec![expected("memory_measurement_ratio", "host=\"draco\"", 0.6)],
        ),
    ];
    let dashboard: Value = serde_json::from_str(include_str!("../../monitoring/dashboard.json"))?;
    let mut covered: std::collections::BTreeSet<_> = entries
        .iter()
        .map(|(name, _)| format!("ix_build:{name}"))
        .collect();
    // Kache expressions are exercised in monitoring/alerts.test.yml.
    covered.extend(
        [
            "ix_kache:hit_ratio",
            "ix_kache:store_fill_ratio",
            "ix_kache:evicted_bytes_1h",
            "ix_kache:permit_wait_share",
            "ix_kache_up",
        ]
        .map(str::to_owned),
    );
    covered.extend(
        [
            // Aggregated panel queries have hand-written dashboard.test.yml fixtures.
            "sum(ix_kache:hits_1h",
            "sum(ix_kache:misses_1h",
            "sum by(host) (ix_kache:misses_1h",
            "ix_kache:miss_ratio",
            "ix_kache:hits_1h",
            "ix_kache:misses_1h",
            "ix_kache:time_saved_seconds_1h",
            "ix_kache:local_hits_1h",
            "ix_kache:remote_hits_1h",
            "ix_build:run_p50_seconds",
            "ix_build:run_p95_seconds",
            "sum by(host) (ix_build:builds_1h",
            "sum by(code) (ix_build:failures_1h",
            "ix_build:storage_used_ratio",
            "ix_build:builds_1h",
            "ix_build:failures_1h",
            "ix_build:build_filesystem_avail_bytes",
            "ix_build:build_device_io_utilisation_pct",
            "ix_kache_store_bytes",
            "ix_kache_store_limit_bytes",
            "ix_kache_entries",
            "ix_kache_remote_reachable",
        ]
        .map(str::to_owned),
    );
    let panels = dashboard["panels"]
        .as_array()
        .ok_or_else(|| Error::new(ErrorCode::State, "dashboard panels missing"))?;
    assert_eq!(panels.iter().filter(|p| p["type"] == "row").count(), 3);
    for panel in panels {
        if let Some(targets) = panel["targets"].as_array() {
            for target in targets {
                let expression = target["expr"].as_str().unwrap_or_default();
                assert!(
                    covered.contains(expression.split('{').next().unwrap_or_default()),
                    "untested panel {expression}"
                );
            }
        }
    }
    let tests=entries.into_iter().map(|(name,values)|json!({"expr":format!("round(ix_build:{name} * 1000000) / 1000000"),"eval_time":"20m","exp_samples":values})).collect::<Vec<_>>();
    let root = tempfile::tempdir()?;
    let rules = root.path().join("alerts.yml");
    fs::write(&rules, include_str!("../../monitoring/alerts.yml"))?;
    let fixture = root.path().join("replay.test.yml");
    fs::write(
        &fixture,
        serde_json::to_vec_pretty(
            &json!({"rule_files":[rules],"evaluation_interval":"1m","fuzzy_compare":true,"tests":[{"name":"replayed ledger questions","interval":"1m","input_series":replay_series()?,"promql_expr_test":tests}]}),
        )?,
    )?;
    let promtool = std::env::var_os("IX_BUILD_PROMTOOL")
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "IX_BUILD_PROMTOOL required"))?;
    let output = Command::new(&promtool)
        .args(["test", "rules"])
        .arg(fixture)
        .output()?;
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut check = Command::new(promtool)
        .args(["check", "metrics"])
        .stdin(Stdio::piped())
        .spawn()?;
    use std::io::Write;
    check
        .stdin
        .take()
        .ok_or_else(|| Error::new(ErrorCode::Io, "promtool stdin"))?
        .write_all(
            super::tests::state(vec![])
                .metrics
                .exposition(&state(vec![]), &policy()?, 0, 0)?
                .as_bytes(),
        )?;
    assert!(check.wait()?.success());
    Ok(())
}
