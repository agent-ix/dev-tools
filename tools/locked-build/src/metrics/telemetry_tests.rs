// SPDX-License-Identifier: MIT
//! Hand-calculated replay and cardinality fixtures for SWM-417.
use super::tests::{event, policy, request, state};
use super::*;
use crate::model::{MemoryMetric, MemorySample, Phase};
use locked_build::wire::Code;

pub(super) fn sample(text: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    let values: Vec<_> = text
        .lines()
        .filter(|line| {
            line.starts_with(&format!("{name}{{"))
                && labels
                    .iter()
                    .all(|(k, v)| line.contains(&format!("{k}=\"{v}\"")))
        })
        .collect();
    assert_eq!(values.len(), 1, "{name} {labels:?}: {values:?}");
    values[0]
        .rsplit_once(' ')
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or_else(|| panic!("invalid sample"))
}

/// Trace: FR-015-AC-1, FR-015-AC-3, FR-015-AC-10
#[test]
fn telemetry_replay_is_exactly_once_after_restore_and_pruning() -> Result<()> {
    let mut e = event(1, "failed", 2, 2500);
    e.owner = "draco:tooling:telemetry2".into();
    e.ticket = "NEVER-LABEL-ME".into();
    e.units = 2;
    e.hold_ms = Some(4000);
    e.trace.running_at_ms = Some(100);
    e.trace.running_seconds = 3.;
    e.trace.diagnostic.code = Some(Code::CommandExit);
    e.telemetry = crate::telemetry::EventTelemetry {
        repo: "dev-tools".into(),
        subcommand: "test".into(),
        reservation_bytes: Some(100),
        memory_sample: Some(MemorySample {
            bytes: 150,
            metric: MemoryMetric::Rss,
            contributors: vec![],
        }),
        ..Default::default()
    };
    let mut metrics = MetricsState::default();
    metrics.reconcile_events(&[e.clone()], 2)?;
    metrics.reconcile_events(&[e], 2)?;
    let restored: MetricsState = serde_json::from_slice(&serde_json::to_vec(&metrics)?)?;
    let current = state(vec![]);
    let text = restored.exposition(&current, &policy()?, 12000, 12000)?;
    for (name, expected) in [
        ("ix_build_wait_seconds_sum", 2.5),
        ("ix_build_run_seconds_sum", 3.),
        ("ix_build_hold_seconds_sum", 4.),
        ("ix_build_memory_reserved_bytes_sum", 100.),
        ("ix_build_memory_peak_bytes_sum", 150.),
        ("ix_build_lane_unit_seconds_total", 8.),
        ("ix_build_repo_run_seconds_total", 3.),
        ("ix_build_repo_run_count_total", 1.),
        ("ix_build_identity_total", 1.),
        ("ix_build_failures_total", 1.),
        ("ix_build_builds_total", 1.),
    ] {
        assert_eq!(sample(&text, name, &[]), expected, "{name}");
    }
    assert_eq!(
        sample(&text, "ix_build_wait_seconds_bucket", &[("le", "1")]),
        0.
    );
    assert_eq!(
        sample(&text, "ix_build_wait_seconds_bucket", &[("le", "10")]),
        1.
    );
    assert!(!text.contains("NEVER-LABEL-ME"));
    assert!(!text.contains("ix_build_repo_run_seconds_bucket"));
    assert!(!text.contains("ix_build_duration_seconds_bucket"));
    let status = restored.snapshot_with_policy(&current, &policy()?, 12000, 12000)?;
    let mut rendered = String::new();
    crate::telemetry::render(&status.telemetry, &mut rendered);
    assert!(text.ends_with(&rendered));
    Ok(())
}

/// Trace: FR-015-AC-1, FR-015-AC-3
#[test]
fn telemetry_current_queue_age_is_per_class_and_keeps_reboot_age() -> Result<()> {
    let mut running = request(1, "draco:tooling:one".into(), Phase::Running);
    running.started_clock_ms = Some(1000);
    running.units = 2;
    let mut queued = request(2, "draco:tooling:one".into(), Phase::Queued);
    queued.class = "point".into();
    queued.enqueued_clock_ms = 2000;
    queued.active_blocked_reasons = vec!["memory_budget".into(), "units".into()];
    let current = state(vec![running, queued]);
    let mut metrics = MetricsState::default();
    metrics.record_pre_reboot_wait("store:r2", 3000);
    let text = metrics.exposition(&current, &policy()?, 10000, 10000)?;
    for (name, expected) in [
        ("ix_build_oldest_running_seconds", 9.),
        ("ix_build_units_used", 2.),
        ("ix_build_policy_hold_seconds", 5400.),
        ("ix_build_policy_maximum_load", 8.),
        ("ix_build_info", 1.),
        ("ix_build_telemetry_dropped_total", 0.),
    ] {
        assert_eq!(sample(&text, name, &[]), expected);
    }
    assert_eq!(
        sample(
            &text,
            "ix_build_oldest_queued_seconds",
            &[("class", "point")]
        ),
        11.
    );
    assert_eq!(
        sample(
            &text,
            "ix_build_oldest_queued_seconds",
            &[("class", "periodic")]
        ),
        0.
    );
    for reason in ["units", "memory_budget"] {
        assert_eq!(
            sample(
                &text,
                "ix_build_queue_depth_by_reason",
                &[("class", "point"), ("reason", reason)]
            ),
            1.
        );
    }
    for removed in [
        "ix_build_requests_total",
        "ix_build_terminations_total",
        "ix_build_blocked_seconds_total",
        "ix_build_queue_wait_seconds",
        "ix_build_slots_busy",
        "ix_build_slots_configured",
        "ix_build_queue_depth{",
        "ix_build_lane_running",
        "ix_build_policy_jobs",
        "aborted_swap",
        "pause_exceeded",
    ] {
        assert!(!text.contains(removed), "{removed}");
    }
    Ok(())
}

/// Trace: FR-015-AC-1, FR-015-AC-3, FR-015-AC-10
#[test]
fn telemetry_wait_uses_saved_age_once_across_admission_start_and_terminal() -> Result<()> {
    let mut metrics = MetricsState::default();
    metrics.record_pre_reboot_wait("store:r1", 4000);
    let admitted = event(1, "running", 0, 3000);
    let mut started = event(2, "running", 0, 3000);
    started.kind = "command_started".into();
    started.request_id = admitted.request_id.clone();
    let mut done = event(3, "succeeded", 0, 3000);
    done.request_id = admitted.request_id.clone();
    metrics.reconcile_events(&[admitted, started, done], 4)?;
    let text = metrics.exposition(&state(vec![]), &policy()?, 0, 0)?;
    assert_eq!(sample(&text, "ix_build_wait_seconds_sum", &[]), 7.);
    let mut reboot = event(4, "failed", 0, 1000);
    reboot.reason = Some("host_rebooted".into());
    reboot.hold_ms = Some(999000);
    reboot.trace.running_at_ms = Some(1);
    reboot.trace.running_seconds = 999.;
    metrics.reconcile_events(&[reboot], 5)?;
    let text = metrics.exposition(&state(vec![]), &policy()?, 0, 0)?;
    assert!(!text.contains("# TYPE ix_build_hold_seconds"));
    assert!(!text.contains("# TYPE ix_build_run_seconds"));
    Ok(())
}

/// Trace: FR-015-AC-1, FR-015-AC-3
#[test]
fn telemetry_generated_full_budget_is_below_1500_series() -> Result<()> {
    let mut metrics = MetricsState::default();
    let mut sequence = 0;
    for repo in 0..30 {
        for subcommand in ["build", "test", "check", "clippy", "xtask", "other"] {
            for phase in ["succeeded", "failed", "cancelled"] {
                sequence += 1;
                let mut e = event(sequence, phase, 0, 1000);
                e.class = PriorityClass::ALL[usize::try_from(sequence % 6).unwrap_or(0)]
                    .label()
                    .into();
                e.owner = format!("draco:tooling:lane-{}", sequence % 64);
                e.telemetry.repo = format!("repo-{repo}");
                e.telemetry.subcommand = subcommand.into();
                e.hold_ms = Some(4000);
                e.trace.running_at_ms = Some(1);
                e.trace.running_seconds = 3.;
                e.telemetry.reservation_bytes = Some(1_000_000_000);
                e.telemetry.memory_sample = Some(MemorySample {
                    bytes: 800_000_000,
                    metric: MemoryMetric::Rss,
                    contributors: vec![],
                });
                metrics.reconcile_events(&[e], sequence + 1)?;
            }
        }
    }
    for class in PriorityClass::ALL {
        for code in Code::ALL {
            sequence += 1;
            let mut e = event(sequence, "failed", 0, 0);
            e.class = class.label().into();
            e.trace.diagnostic.code = Some(code);
            metrics.reconcile_events(&[e], sequence + 1)?;
        }
    }
    for class in PriorityClass::ALL {
        for phase in ["succeeded", "failed", "cancelled", "timed_out"] {
            sequence += 1;
            let mut e = event(sequence, phase, 0, 0);
            e.class = class.label().into();
            e.trace.running_at_ms = Some(1);
            e.trace.running_seconds = 1.;
            e.hold_ms = Some(1000);
            if phase == "timed_out" {
                e.trace.diagnostic.code = Some(Code::HoldTimeout);
            }
            metrics.reconcile_events(&[e], sequence + 1)?;
        }
    }
    let mut active = Vec::new();
    for class in PriorityClass::ALL {
        for phase in [Phase::Queued, Phase::Running] {
            let mut row = request(
                u64::try_from(active.len()).unwrap_or(0) + 1,
                "draco:tooling:active".into(),
                phase,
            );
            row.class = class.label().into();
            row.memory_reservation_bytes = Some(42);
            row.memory_cap_bytes = Some(64);
            active.push(row);
        }
    }
    let text = metrics.exposition(&state(active), &policy()?, 0, 0)?;
    for class in PriorityClass::ALL {
        for phase in ["queued", "running"] {
            assert_eq!(
                sample(
                    &text,
                    "ix_build_active_memory_reserved_bytes",
                    &[("class", class.label()), ("state", phase)]
                ),
                42.
            );
            assert_eq!(
                sample(
                    &text,
                    "ix_build_active_memory_limit_bytes",
                    &[("class", class.label()), ("state", phase)]
                ),
                64.
            );
        }
    }
    let series = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .count();
    eprintln!("SWM-417 generated series={series} (+6 runtime/exporter gauges)");
    assert!(series + 9 <= 1500, "series budget exceeded: {}", series + 9);
    assert_eq!(
        text.lines()
            .filter(|l| l.starts_with("ix_build_identity_total{"))
            .count(),
        543
    );
    assert_eq!(
        sample(
            &text,
            "ix_build_identity_total",
            &[
                ("repo", "repo-29"),
                ("subcommand", "other"),
                ("outcome", "other")
            ]
        ),
        1.
    );
    let mut declared = std::collections::BTreeSet::new();
    for (index, span) in include_str!("../../monitoring/metrics.md")
        .split('\x60')
        .enumerate()
    {
        if index % 2 == 1 && span.starts_with("ix_build_") {
            declared.insert(span.split('{').next().unwrap_or(span).to_owned());
        }
    }
    let mut exported = text
        .lines()
        .filter_map(|line| line.strip_prefix("# TYPE "))
        .filter_map(|line| line.split_once(' '))
        .map(|(name, _)| name.to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    exported.extend(
        [
            "ix_build_leases_missing_process",
            "ix_build_unleased_build_processes",
            "ix_build_state_dir_free_bytes",
            "ix_build_snapshot_size_bytes",
            "ix_build_export_error",
            "ix_build_running_memory_headroom_ratio",
            "ix_build_queue_paused",
            "ix_build_queue_hold_age_seconds",
            "ix_build_queue_hold_alert_after_seconds",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    assert_eq!(
        declared, exported,
        "metric documentation and exporter families diverged"
    );
    eprintln!("SWM-417 family_count={}", exported.len());
    // Overflow routes to one permanent other lane; ticket never affects attribution.
    sequence += 1;
    let mut overflow = event(sequence, "succeeded", 0, 0);
    overflow.owner = "draco:tooling:overflow".into();
    overflow.hold_ms = Some(1000);
    overflow.telemetry.repo = "repo-overflow".into();
    metrics.reconcile_events(&[overflow], sequence + 1)?;
    let text = metrics.exposition(&state(vec![]), &policy()?, 0, 0)?;
    assert_eq!(
        sample(
            &text,
            "ix_build_lane_unit_seconds_total",
            &[("lane", "other")]
        ),
        25.
    );
    Ok(())
}

/// Trace: FR-015-AC-3, FR-015-AC-10
#[test]
fn telemetry_upgrades_stored_totals_without_reexporting_legacy_labels() -> Result<()> {
    let previous = serde_json::json!({"telemetry":{"families":[{"name":"ix_build_identity_total","help":"old","kind":"counter","samples":[{"suffix":"","labels":{"class":"point","outcome":"cancelled","lane":"draco:tooling:one","ticket":"PRIVATE","repo":"dev-tools","subcommand":"test"},"value":3}]}],"identities":["old tuple"],"lanes":["draco:tooling:one"]},"store_id":"store","last_event_sequence":1});
    let metrics: MetricsState = serde_json::from_value(previous)?;
    let text = metrics.exposition(&state(vec![]), &policy()?, 0, 0)?;
    assert_eq!(
        sample(&text, "ix_build_builds_total", &[("outcome", "cancelled")]),
        3.
    );
    assert_eq!(
        sample(&text, "ix_build_identity_total", &[("outcome", "other")]),
        3.
    );
    assert!(!text.contains("ticket="));
    assert!(!text.contains("PRIVATE"));
    Ok(())
}

/// Trace: FR-015-AC-10
#[test]
fn telemetry_atomic_publication_preserves_inode_when_content_is_unchanged() -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir()?;
    let output = root.path().join("same.prom");
    write_atomic(&output, "metric 1\n")?;
    let before = fs::metadata(&output)?;
    write_atomic(&output, "metric 1\n")?;
    let after = fs::metadata(&output)?;
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.modified()?, after.modified()?);
    write_atomic(&output, "metric 2\n")?;
    assert_ne!(before.ino(), fs::metadata(&output)?.ino());
    assert_eq!(fs::read_to_string(output)?, "metric 2\n");
    Ok(())
}

/// Provenance: SWM-425; compile with the documented fixed provenance inputs for an independent label oracle.
#[test]
#[ignore = "build with IX_BUILD_COMMIT=1111111111111111111111111111111111111111 IX_BUILD_DIRTY=false"]
fn provenance_info_contains_hand_written_compile_labels() -> Result<()> {
    let text = MetricsState::default().exposition(&state(vec![]), &policy()?, 0, 0)?;
    let info = text
        .lines()
        .find(|line| line.starts_with("ix_build_info{"))
        .ok_or_else(|| Error::new(ErrorCode::State, "missing info metric"))?;
    assert!(info.contains("commit=\"1111111111111111111111111111111111111111\""));
    assert!(info.contains("dirty=\"false\""));
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("ix_build_info{"))
            .count(),
        1
    );
    Ok(())
}
