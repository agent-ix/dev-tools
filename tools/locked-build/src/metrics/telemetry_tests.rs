// SPDX-License-Identifier: MIT
//! Hand-calculated telemetry acceptance fixtures (SWM-405).

use super::tests::{event, policy, request, state};
use super::*;
use crate::model::{MemoryMetric, MemorySample, Phase};
use locked_build::wire::Code;

fn sample(text: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    let values: Vec<_> = text
        .lines()
        .filter(|line| {
            line.starts_with(&format!("{name}{{"))
                && labels
                    .iter()
                    .all(|(key, value)| line.contains(&format!("{key}=\"{value}\"")))
        })
        .collect();
    assert_eq!(values.len(), 1, "{name} {labels:?}: {values:?}");
    values[0]
        .rsplit_once(' ')
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or_else(|| panic!("invalid sample {}", values[0]))
}

/// Trace: FR-015-AC-1, FR-015-AC-3, FR-015-AC-10
/// Provenance: SWM-405
#[test]
fn telemetry_histograms_failures_identity_and_ledger_have_hand_written_values() -> Result<()> {
    let mut done = event(1, "failed", 2, 2_500);
    done.owner = "draco:tooling:telemetry".into();
    done.ticket = "SWM-405".into();
    done.hold_ms = Some(4_000);
    done.trace.running_at_ms = Some(100);
    done.trace.running_seconds = 3.0;
    done.trace.diagnostic.code = Some(Code::CommandExit);
    done.telemetry = crate::telemetry::EventTelemetry {
        repo: "dev-tools".into(),
        subcommand: "test".into(),
        exit_code: Some(101),
        reservation_bytes: Some(100),
        limit_bytes: Some(200),
        hold_limit_ms: Some(8_000),
        memory_sample: Some(MemorySample {
            bytes: 150,
            metric: MemoryMetric::Rss,
            contributors: vec![],
        }),
        ..Default::default()
    };
    let mut metrics = MetricsState::default();
    metrics.reconcile_events(&[done.clone()], 2)?;
    metrics.reconcile_events(&[done], 2)?;
    let metrics: MetricsState = serde_json::from_slice(&serde_json::to_vec(&metrics)?)?;
    let current = state(vec![]); // Requests/events pruned; the ledger alone owns totals.
    let text = metrics.exposition(&current, &policy()?, 12_000, 12_000)?;
    let labels = [("class", "point"), ("outcome", "failed")];
    for (name, expected) in [
        ("ix_build_wait_seconds_sum", 2.5),
        ("ix_build_wait_seconds_count", 1.0),
        ("ix_build_run_seconds_sum", 3.0),
        ("ix_build_run_seconds_count", 1.0),
        ("ix_build_hold_seconds_sum", 4.0),
        ("ix_build_hold_limit_ratio_sum", 0.5),
        ("ix_build_memory_reserved_bytes_sum", 100.0),
        ("ix_build_memory_peak_bytes_sum", 150.0),
        ("ix_build_memory_headroom_ratio_sum", 0.25),
    ] {
        assert_eq!(sample(&text, name, &labels), expected, "{name}");
    }
    assert_eq!(
        sample(
            &text,
            "ix_build_wait_seconds_bucket",
            &[("class", "point"), ("outcome", "failed"), ("le", "1")]
        ),
        0.0
    );
    assert_eq!(
        sample(
            &text,
            "ix_build_wait_seconds_bucket",
            &[("class", "point"), ("outcome", "failed"), ("le", "5")]
        ),
        1.0
    );
    assert_eq!(
        sample(
            &text,
            "ix_build_failures_total",
            &[
                ("code", "LB_COMMAND_EXIT"),
                ("exit_class", "101"),
                ("signal", "none"),
                ("cause", "command")
            ]
        ),
        1.0
    );
    assert_eq!(
        sample(
            &text,
            "ix_build_identity_total",
            &[
                ("lane", "draco:tooling:telemetry"),
                ("ticket", "SWM-405"),
                ("repo", "dev-tools"),
                ("subcommand", "test")
            ]
        ),
        1.0
    );
    Ok(())
}

/// Trace: FR-015-AC-1, FR-015-AC-10
/// Provenance: SWM-405
#[test]
fn telemetry_capacity_policy_and_host_series_match_current_state() -> Result<()> {
    let mut cfg = policy()?;
    cfg.capacity_units = 8;
    cfg.memory_budget_bytes = Some(1_000);
    let mut running = request(1, "draco:tooling:telemetry".into(), Phase::Running);
    running.units = 2;
    running.memory_reservation_bytes = Some(300);
    running.memory_cap_bytes = Some(800);
    running.started_clock_ms = Some(1_000);
    running.lease_record = Some(crate::model::LeaseRecord {
        memory_sample: Some(MemorySample {
            bytes: 200,
            metric: MemoryMetric::Rss,
            contributors: vec![],
        }),
        ..Default::default()
    });
    let mut queued = request(2, "draco:tooling:telemetry".into(), Phase::Queued);
    queued.enqueued_clock_ms = 2_000;
    queued.active_blocked_reasons = vec!["memory_budget".into(), "units".into()];
    let current = state(vec![running, queued]);
    let text = current.metrics.exposition(&current, &cfg, 10_000, 10_000)?;
    for (name, expected) in [
        ("ix_build_running", 1.0),
        ("ix_build_units_used", 2.0),
        ("ix_build_units_total", 8.0),
        ("ix_build_memory_budget_used_bytes", 300.0),
        ("ix_build_memory_budget_total_bytes", 1_000.0),
        ("ix_build_running_memory_headroom_ratio", 0.75),
        ("ix_build_oldest_queued_seconds", 8.0),
        ("ix_build_policy_hold_seconds", 5_400.0),
        ("ix_build_policy_jobs", 4.0),
        ("ix_build_policy_test_threads", 4.0),
        ("ix_build_policy_memory_reserve_bytes", 2_147_483_648.0),
        (
            "ix_build_policy_memory_reserve_per_job_bytes",
            3_000_000_000.0,
        ),
        (
            "ix_build_policy_memory_limit_per_job_bytes",
            8_000_000_000.0,
        ),
        ("ix_build_policy_maximum_load", 8.0),
        ("ix_build_policy_disk_reserve_bytes", 21_474_836_480.0),
        ("ix_build_policy_maximum_units", 1.0),
        ("ix_build_export_timestamp_seconds", 10.0),
        ("ix_build_supervisor_restarts_total", 0.0),
        ("ix_build_recover_events_total", 0.0),
        ("ix_build_boot_changes_total", 0.0),
        ("ix_build_info", 1.0),
    ] {
        assert_eq!(sample(&text, name, &[]), expected, "{name}");
    }
    for reason in ["memory_budget", "units"] {
        assert_eq!(
            sample(
                &text,
                "ix_build_queue_depth_by_reason",
                &[("class", "periodic"), ("reason", reason)]
            ),
            1.0
        );
    }
    assert_eq!(
        sample(
            &text,
            "ix_build_owner_queued",
            &[("lane", "draco:tooling:telemetry")]
        ),
        1.0
    );
    Ok(())
}

/// Trace: FR-015-AC-1, FR-015-AC-3, FR-015-AC-10
/// Provenance: SWM-405
#[test]
fn telemetry_all_codes_signals_kill_measurements_and_system_events_are_durable() -> Result<()> {
    let mut events = Vec::new();
    for (i, code) in Code::ALL.into_iter().enumerate() {
        let mut e = event(
            u64::try_from(i).map_err(|_| Error::new(ErrorCode::State, "fixture index"))? + 1,
            "failed",
            0,
            0,
        );
        e.trace.diagnostic.code = Some(code);
        e.telemetry.exit_code = Some(75);
        e.telemetry.signal = Some(9);
        e.telemetry.limit_bytes = Some(100);
        e.telemetry.memory_sample = Some(MemorySample {
            bytes: 125,
            metric: MemoryMetric::PssAnon,
            contributors: vec![],
        });
        events.push(e);
    }
    let mut current = state(vec![]);
    current.events = events;
    current.next_event = 14;
    for kind in ["supervisor_restart", "recovered", "boot_changed"] {
        current.system_event(kind, &policy()?, 1)?;
    }
    current
        .metrics
        .reconcile_events(&current.events, current.next_event)?;
    let text = current.metrics.exposition(&current, &policy()?, 1, 1)?;
    for code in Code::ALL {
        assert_eq!(
            sample(
                &text,
                "ix_build_failures_total",
                &[
                    ("code", code.as_str()),
                    ("exit_class", "other"),
                    ("signal", "9")
                ]
            ),
            1.0
        );
    }
    for code in ["LB_RSS_CAP", "LB_OOM_KILLED"] {
        assert_eq!(
            sample(
                &text,
                "ix_build_memory_kill_limit_bytes_sum",
                &[("code", code)]
            ),
            100.0
        );
        assert_eq!(
            sample(
                &text,
                "ix_build_memory_kill_measured_bytes_sum",
                &[("code", code)]
            ),
            125.0
        );
    }
    for name in [
        "ix_build_supervisor_restarts_total",
        "ix_build_recover_events_total",
        "ix_build_boot_changes_total",
    ] {
        assert_eq!(sample(&text, name, &[]), 1.0);
    }
    assert_eq!(
        sample(&text, "ix_build_memory_headroom_ratio_sum", &[]),
        -3.25
    );
    Ok(())
}

/// Trace: FR-015-AC-1, FR-015-AC-3
/// Provenance: SWM-405
#[test]
fn telemetry_identity_and_owner_cardinality_stays_bounded_across_pruning() -> Result<()> {
    let mut metrics = MetricsState::default();
    for sequence in 1..=200 {
        let mut e = event(sequence, "succeeded", 0, 0);
        e.owner = format!("draco:tooling:lane-{sequence}");
        e.ticket = format!("SWM-{sequence}");
        e.telemetry.repo = format!("repo-{sequence}");
        e.telemetry.subcommand = "test --secret=NEVER".into();
        metrics.reconcile_events(&[e], sequence + 1)?;
    }
    let current = state(vec![]);
    let text = metrics.exposition(&current, &policy()?, 0, 0)?;
    assert_eq!(
        text.lines()
            .filter(|l| l.starts_with("ix_build_identity_total{"))
            .count(),
        129
    );
    assert_eq!(
        sample(
            &text,
            "ix_build_identity_total",
            &[("lane", "other"), ("ticket", "other"), ("repo", "other")]
        ),
        72.0
    );
    assert_eq!(
        text.lines()
            .filter(|l| l.starts_with("ix_build_owner_queued{"))
            .count(),
        65
    );
    assert!(!text.contains("NEVER"));
    let restored: MetricsState = serde_json::from_slice(&serde_json::to_vec(&metrics)?)?;
    assert_eq!(restored.exposition(&current, &policy()?, 0, 0)?, text);
    Ok(())
}

/// Trace: FR-015-AC-1, FR-015-AC-3
/// Provenance: SWM-405
#[test]
fn telemetry_status_and_text_use_identical_shared_samples() -> Result<()> {
    let mut current = state(vec![]);
    let e = event(1, "succeeded", 0, 1_500);
    current.metrics.reconcile_events(&[e], 2)?;
    let cfg = policy()?;
    let status = current
        .metrics
        .snapshot_with_policy(&current, &cfg, 2_000, 2_000);
    let decoded: locked_build::wire::MetricsSnapshot =
        serde_json::from_slice(&serde_json::to_vec(&status)?)?;
    let mut rendered = String::new();
    crate::telemetry::render(&decoded.telemetry, &mut rendered);
    let text = current.metrics.exposition(&current, &cfg, 2_000, 2_000)?;
    assert!(text.ends_with(&rendered));
    assert_eq!(sample(&text, "ix_build_wait_seconds_sum", &[]), 1.5);
    Ok(())
}

/// Trace: FR-015-AC-1
/// Provenance: SWM-405
#[test]
fn telemetry_class_policy_and_identity_parsing_are_exact() -> Result<()> {
    let mut cfg = policy()?;
    cfg.memory_reserve_per_job_by_class
        .insert("point".into(), 100);
    cfg.memory_per_job_by_class.insert("point".into(), 200);
    cfg.memory_cap_by_class.insert("point".into(), 300);
    let current = state(vec![]);
    let text = current.metrics.exposition(&current, &cfg, 0, 0)?;
    for (name, expected) in [
        ("ix_build_policy_class_memory_reserve_per_job_bytes", 100.0),
        ("ix_build_policy_class_memory_limit_per_job_bytes", 200.0),
        ("ix_build_policy_class_memory_cap_bytes", 300.0),
    ] {
        assert_eq!(sample(&text, name, &[("class", "point")]), expected);
    }
    assert_eq!(
        sample(&text, "ix_build_memory_measurements_available", &[]),
        0.0
    );
    assert!(!text.contains("# TYPE ix_build_running_memory_headroom_ratio"));
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join(".git"))?;
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested)?;
    for (command, expected) in [
        (
            vec!["cargo", "+stable", "--color", "always", "test", "--secret"],
            "test",
        ),
        (vec!["cargo", "--help", "test"], "other"),
        (vec!["cargo", "--config=never-label-me", "clippy"], "clippy"),
    ] {
        let identity = crate::telemetry::EventTelemetry::identity(
            &nested,
            &command.into_iter().map(str::to_owned).collect::<Vec<_>>(),
        );
        assert_eq!(identity.subcommand, expected);
        assert_eq!(
            identity.repo,
            root.path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
    }
    Ok(())
}
