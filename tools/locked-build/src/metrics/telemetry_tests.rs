// SPDX-License-Identifier: MIT
//! Hand-calculated telemetry acceptance fixtures (SWM-405).

use super::tests::{event, policy, request, state};
use super::*;
use crate::model::{MemoryMetric, MemorySample, Phase};
use locked_build::wire::Code;

fn sample(text: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    let values: Vec<_> = text.lines().filter(|line| {
        line.starts_with(&format!("{name}{{")) && labels.iter().all(|(key, value)| line.contains(&format!("{key}=\"{value}\"")))
    }).collect();
    assert_eq!(values.len(), 1, "{name} {labels:?}: {values:?}");
    values[0].rsplit_once(' ').and_then(|(_, v)| v.parse().ok()).unwrap_or_else(|| panic!("invalid sample {}", values[0]))
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
        repo: "dev-tools".into(), subcommand: "test".into(), exit_code: Some(101),
        reservation_bytes: Some(100), limit_bytes: Some(200), hold_limit_ms: Some(8_000),
        memory_sample: Some(MemorySample { bytes: 150, metric: MemoryMetric::Rss, contributors: vec![] }),
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
        ("ix_build_wait_seconds_sum", 2.5), ("ix_build_wait_seconds_count", 1.0),
        ("ix_build_run_seconds_sum", 3.0), ("ix_build_run_seconds_count", 1.0),
        ("ix_build_hold_seconds_sum", 4.0), ("ix_build_hold_limit_ratio_sum", 0.5),
        ("ix_build_memory_reserved_bytes_sum", 100.0), ("ix_build_memory_peak_bytes_sum", 150.0),
        ("ix_build_memory_headroom_ratio_sum", 0.25),
    ] { assert_eq!(sample(&text, name, &labels), expected, "{name}"); }
    assert_eq!(sample(&text, "ix_build_wait_seconds_bucket", &[("class", "point"), ("outcome", "failed"), ("le", "1")]), 0.0);
    assert_eq!(sample(&text, "ix_build_wait_seconds_bucket", &[("class", "point"), ("outcome", "failed"), ("le", "5")]), 1.0);
    assert_eq!(sample(&text, "ix_build_failures_total", &[("code", "LB_COMMAND_EXIT"), ("exit_class", "101"), ("signal", "none"), ("cause", "command")]), 1.0);
    assert_eq!(sample(&text, "ix_build_identity_total", &[("lane", "draco:tooling:telemetry"), ("ticket", "SWM-405"), ("repo", "dev-tools"), ("subcommand", "test")]), 1.0);
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
    running.lease_record = Some(crate::model::LeaseRecord { memory_sample: Some(MemorySample { bytes: 200, metric: MemoryMetric::Rss, contributors: vec![] }), ..Default::default() });
    let mut queued = request(2, "draco:tooling:telemetry".into(), Phase::Queued);
    queued.enqueued_clock_ms = 2_000;
    queued.active_blocked_reasons = vec!["memory_budget".into(), "units".into()];
    let current = state(vec![running, queued]);
    let text = current.metrics.exposition(&current, &cfg, 10_000, 10_000)?;
    for (name, expected) in [
        ("ix_build_running", 1.0), ("ix_build_units_used", 2.0), ("ix_build_units_total", 8.0),
        ("ix_build_memory_budget_used_bytes", 300.0), ("ix_build_memory_budget_total_bytes", 1_000.0),
        ("ix_build_memory_headroom_ratio", 0.75), ("ix_build_oldest_queued_seconds", 8.0),
        ("ix_build_policy_hold_seconds", 5_400.0), ("ix_build_policy_jobs", 4.0),
        ("ix_build_policy_test_threads", 4.0), ("ix_build_policy_memory_reserve_bytes", 2_147_483_648.0),
        ("ix_build_policy_memory_reserve_per_job_bytes", 3_000_000_000.0),
        ("ix_build_policy_memory_limit_per_job_bytes", 8_000_000_000.0),
        ("ix_build_policy_maximum_load", 8.0), ("ix_build_policy_disk_reserve_bytes", 21_474_836_480.0),
        ("ix_build_policy_maximum_units", 1.0), ("ix_build_export_timestamp_seconds", 10.0),
        ("ix_build_supervisor_restarts_total", 0.0), ("ix_build_recover_events_total", 0.0),
        ("ix_build_boot_changes_total", 0.0), ("ix_build_info", 1.0),
    ] { assert_eq!(sample(&text, name, &[]), expected, "{name}"); }
    for reason in ["memory_budget", "units"] {
        assert_eq!(sample(&text, "ix_build_queue_depth_by_reason", &[("class", "periodic"), ("reason", reason)]), 1.0);
    }
    assert_eq!(sample(&text, "ix_build_owner_queued", &[("lane", "draco:tooling:telemetry")]), 1.0);
    Ok(())
}
