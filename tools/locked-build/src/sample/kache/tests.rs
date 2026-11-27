// SPDX-License-Identifier: MIT
use super::*;

fn fixtures() -> Result<(Value, Value, Value)> {
    Ok((
        serde_json::from_str(include_str!("../fixtures/luna-stats.json"))?,
        serde_json::from_str(include_str!("../fixtures/luna-config.json"))?,
        serde_json::from_str(include_str!("../fixtures/luna-explain.json"))?,
    ))
}
fn value(text: &str, name: &str) -> Result<f64> {
    text.lines()
        .find(|s| s.starts_with(&format!("ix_kache_{name}{{")))
        .and_then(|s| s.rsplit_once(' '))
        .and_then(|(_, v)| v.parse().ok())
        .ok_or_else(unavailable)
}

/// Provenance: SWM-416, real luna 2.4.0 output; independent numeric oracles.
#[test]
fn kache_real_local_fixture_has_hand_written_values_and_bounded_series() -> Result<()> {
    let (stats, config, explain) = fixtures()?;
    let mut ledger = Ledger::default();
    let mut text = String::new();
    render(&stats, &config, &explain, "luna", &mut ledger, &mut text)?;
    assert_eq!(value(&text, "store_bytes")?, 76316814781.);
    assert_eq!(value(&text, "store_limit_bytes")?, 85899345920.);
    assert_eq!(value(&text, "entries")?, 498.);
    assert_eq!(value(&text, "hits_total")?, 473.);
    assert_eq!(value(&text, "misses_total")?, 543.);
    assert_eq!(value(&text, "remote_hits_total")?, 0.);
    assert_eq!(value(&text, "hit_compile_seconds_total")?, 2858.574);
    assert_eq!(value(&text, "miss_compile_seconds_total")?, 7557.079);
    assert_eq!(value(&text, "permit_wait_seconds_total")?, 204.126);
    assert_eq!(value(&text, "gc_evicted_entries_total")?, 47.);
    assert_eq!(value(&text, "gc_evicted_bytes_total")?, 93358089730.);
    assert_eq!(value(&text, "counter_epoch")?, 1795809140.);
    assert_eq!(value(&text, "gc_last_run_timestamp")?, 1795811493.);
    assert!(text.contains("store=\"local\",version=\"2.4.0\""));
    assert!(text.contains("reason=\"not_a_compile\"} 12"));
    assert!(text.contains("reason=\"unsupported\"} 8"));
    let series = text.lines().filter(|s| !s.starts_with('#')).count();
    assert_eq!(series, 20);
    assert!(series <= 60);
    Ok(())
}

/// Provenance: SWM-416; synthetic remote variant of the redacted real fixture.
#[test]
fn kache_shared_fixture_remote_hits_and_unbounded_reasons_stay_within_budget() -> Result<()> {
    let (_, _, mut explain) = fixtures()?;
    let stats: Value = serde_json::from_str(include_str!("../fixtures/shared-stats.json"))?;
    let mut config: Value = serde_json::from_str(include_str!("../fixtures/shared-config.json"))?;
    explain["reasons"] = (0..1000).map(|i| serde_json::json!({"result":"passthrough","reason":format!("unbounded-{i}"),"count":1})).collect();
    let mut text = String::new();
    render(
        &stats,
        &config,
        &explain,
        "luna",
        &mut Ledger::default(),
        &mut text,
    )?;
    assert_eq!(value(&text, "remote_hits_total")?, 25.);
    assert_eq!(value(&text, "hits_total")?, 498.);
    assert!(text.contains("store=\"shared\""));
    assert!(text.contains("reason=\"other\"} 1000"));
    assert!(!text.contains("unbounded"));
    assert!(text.lines().filter(|s| !s.starts_with('#')).count() <= 60);
    config["values"]["cache"]["local_only"] = true.into();
    assert_eq!(store_label(&config["values"])?, "local");
    Ok(())
}

/// Provenance: SWM-416; exporter restarts, daemon epochs and repeated GC snapshots.
#[test]
fn kache_counters_survive_serialization_and_source_resets_without_gc_double_counting() -> Result<()>
{
    let (mut stats, config, explain) = fixtures()?;
    let mut ledger = Ledger::default();
    render(
        &stats,
        &config,
        &explain,
        "luna",
        &mut ledger,
        &mut String::new(),
    )?;
    let mut restored: Ledger = serde_json::from_str(&serde_json::to_string(&ledger)?)?;
    let mut text = String::new();
    render(&stats, &config, &explain, "luna", &mut restored, &mut text)?;
    assert_eq!(value(&text, "hits_total")?, 473.);
    assert_eq!(value(&text, "gc_evicted_entries_total")?, 47.);
    stats["lifetime"]["since"] = "2026-11-27T21:00:00Z".into();
    stats["local_hits"] = 500.into(); // New history baseline is not added again.
    stats["misses"] = 2.into();
    stats["gc"]["last_run"] = "2026-11-27T21:01:00Z".into();
    stats["gc"]["entries_evicted"] = 3.into();
    stats["gc"]["bytes_freed"] = 100.into();
    text.clear();
    render(&stats, &config, &explain, "luna", &mut restored, &mut text)?;
    assert_eq!(value(&text, "hits_total")?, 473.);
    assert_eq!(value(&text, "misses_total")?, 543.);
    assert_eq!(value(&text, "gc_evicted_entries_total")?, 50.);
    assert_eq!(value(&text, "gc_evicted_bytes_total")?, 93358089830.);
    assert_eq!(value(&text, "counter_epoch")?, 1795813200.);
    Ok(())
}

/// Provenance: SWM-416; malformed inputs must not publish partial metric families.
#[test]
fn kache_rejects_missing_malformed_and_failed_source_fields() -> Result<()> {
    let (stats, config, explain) = fixtures()?;
    for pointer in [
        "/disk/store_bytes",
        "/disk/store_limit_bytes",
        "/lifetime/since",
        "/local_hits",
        "/miss_elapsed_ms",
        "/stores/0/entries",
    ] {
        let mut bad = stats.clone();
        *bad.pointer_mut(pointer).ok_or_else(unavailable)? = Value::Null;
        assert!(
            render(
                &bad,
                &config,
                &explain,
                "luna",
                &mut Ledger::default(),
                &mut String::new()
            )
            .is_err(),
            "{pointer}"
        );
    }
    let mut bad = stats.clone();
    bad["stores"][0]["error"] = "sensitive path failure".into();
    assert!(
        render(
            &bad,
            &config,
            &explain,
            "luna",
            &mut Ledger::default(),
            &mut String::new()
        )
        .is_err()
    );
    let mut no_gc = stats;
    no_gc["gc"] = Value::Null;
    let mut text = String::new();
    render(
        &no_gc,
        &config,
        &explain,
        "luna",
        &mut Ledger::default(),
        &mut text,
    )?;
    assert_eq!(value(&text, "gc_evicted_bytes_total")?, 0.);
    Ok(())
}

/// Provenance: SWM-416; exercise the real process boundary without a shell.
#[test]
fn kache_process_boundary_bounds_hangs_output_and_bad_json() {
    for (binary, args) in [
        ("/bin/sleep", vec!["30"]),
        ("/usr/bin/yes", vec![]),
        ("/usr/bin/printf", vec!["not json"]),
        ("/usr/bin/false", vec![]),
        ("/missing/kache", vec![]),
    ] {
        assert!(
            command(
                Path::new(binary),
                &args,
                Instant::now() + Duration::from_millis(100)
            )
            .is_err()
        );
    }
}

/// Provenance: SWM-416; absence and scrape failures leave unrelated telemetry intact.
#[test]
fn kache_optional_source_is_absent_or_fails_open_without_partial_metrics() -> Result<()> {
    let root = tempfile::tempdir()?;
    let ledger = root.path().join("ledger.json");
    let mut text = "existing telemetry\n".to_owned();
    append_for(
        &mut text,
        "luna",
        &ledger,
        None,
        Some(true),
        Instant::now() + TIMEOUT,
    );
    assert_eq!(text, "existing telemetry\n");
    append_for(
        &mut text,
        "luna",
        &ledger,
        Some(Path::new("/missing/kache")),
        Some(true),
        Instant::now() + TIMEOUT,
    );
    assert!(text.starts_with("existing telemetry\n"));
    assert_eq!(value(&text, "up")?, 0.);
    assert_eq!(value(&text, "scrape_error")?, 1.);
    assert_eq!(value(&text, "daemon_expected")?, 1.);
    assert!(!text.contains("hits_total"));
    assert!(!ledger.exists());
    Ok(())
}

/// Provenance: SWM-416; truncated retained history must not invent activity.
#[test]
fn kache_same_epoch_decrease_rebaselines_without_counting_old_events_twice() -> Result<()> {
    let mut ledger = Ledger::default();
    assert_eq!(ledger.advance("hits_total", 100, true)?, 100);
    assert_eq!(ledger.advance("hits_total", 99, false)?, 100);
    assert_eq!(ledger.advance("hits_total", 100, false)?, 101);
    assert_eq!(ledger.advance("hits_total", 200, true)?, 101);
    Ok(())
}

/// Provenance: SWM-416; competing collectors cannot read stale baselines.
#[test]
fn kache_ledger_lock_is_nonblocking_and_held_until_commit() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("ledger.json");
    let (guard, mut ledger) = read_ledger(&path)?;
    assert!(read_ledger(&path).is_err());
    ledger.advance("hits_total", 100, true)?;
    crate::metrics::write_atomic(&path, &serde_json::to_string(&ledger)?)?;
    assert!(read_ledger(&path).is_err());
    drop(guard);
    let (_next, restored) = read_ledger(&path)?;
    assert_eq!(restored.totals.get("hits_total"), Some(&100));
    Ok(())
}

/// Provenance: SWM-416; schema errors and status failures must fail closed.
#[test]
fn kache_process_boundary_validates_schema_and_success() {
    for document in [
        r#"{"schema_version":1,"success":true}"#,
        r#"{"schema_version":2,"success":true}"#,
        r#"{"schema_version":1,"success":false}"#,
    ] {
        let observed = command(
            Path::new("/usr/bin/printf"),
            &[document],
            Instant::now() + Duration::from_secs(1),
        );
        assert_eq!(
            observed.is_ok(),
            document == r#"{"schema_version":1,"success":true}"#
        );
    }
}

/// Provenance: SWM-416 review; daemon restarts retain history already counted.
#[test]
fn kache_restart_same_history_does_not_replay_counters() -> Result<()> {
    let (stats, config, explain) = fixtures()?;
    let mut ledger = Ledger::default();
    render(
        &stats,
        &config,
        &explain,
        "luna",
        &mut ledger,
        &mut String::new(),
    )?;
    let totals = ledger.totals.clone();
    let mut restart: Value = serde_json::from_str(include_str!("../fixtures/restart-stats.json"))?;
    // Daemon lifetime savings may change independently of retained event history.
    restart["lifetime"]["hit_compile_time_ms"] = 9999999.into();
    let mut text = String::new();
    render(&restart, &config, &explain, "luna", &mut ledger, &mut text)?;
    assert_eq!(ledger.totals, totals);
    assert_eq!(value(&text, "misses_total")?, 543.);
    assert_eq!(value(&text, "hit_compile_seconds_total")?, 2858.574);
    assert_eq!(value(&text, "counter_epoch")?, 1795813200.);
    restart["misses"] = 544.into();
    text.clear();
    render(&restart, &config, &explain, "luna", &mut ledger, &mut text)?;
    assert_eq!(value(&text, "misses_total")?, 544.);
    Ok(())
}

/// Provenance: SWM-416 review; upstream v2.4.0 persists the latest GC run.
#[test]
fn kache_second_gc_run_counts_only_that_run_and_null_last_run_is_available() -> Result<()> {
    let (mut stats, config, explain) = fixtures()?;
    let mut ledger = Ledger::default();
    render(
        &stats,
        &config,
        &explain,
        "luna",
        &mut ledger,
        &mut String::new(),
    )?;
    stats["gc"]["last_run"] = "2026-11-27T22:00:00Z".into();
    stats["gc"]["entries_evicted"] = 3.into();
    stats["gc"]["bytes_freed"] = 100.into();
    let mut text = String::new();
    render(&stats, &config, &explain, "luna", &mut ledger, &mut text)?;
    assert_eq!(value(&text, "gc_evicted_entries_total")?, 50.);
    assert_eq!(value(&text, "gc_evicted_bytes_total")?, 93358089830.);
    let totals = ledger.totals.clone();
    stats["gc"]["last_run"] = Value::Null;
    text.clear();
    render(&stats, &config, &explain, "luna", &mut ledger, &mut text)?;
    assert_eq!(value(&text, "up")?, 1.);
    assert_eq!(value(&text, "scrape_error")?, 0.);
    assert_eq!(value(&text, "gc_last_run_timestamp")?, 0.);
    assert_eq!(ledger.totals, totals);
    Ok(())
}

/// Provenance: SWM-416 review; readonly hits are a subset, remote hits are distinct.
#[test]
fn kache_readonly_hits_are_not_double_counted_and_remote_key_is_nested() -> Result<()> {
    let (mut stats, mut config, explain) = fixtures()?;
    stats["readonly_hits"] = 100.into();
    stats["prefetch_hits"] = 2.into();
    stats["remote_hits"] = 25.into();
    let mut text = String::new();
    render(
        &stats,
        &config,
        &explain,
        "luna",
        &mut Ledger::default(),
        &mut text,
    )?;
    assert_eq!(value(&text, "hits_total")?, 500.);
    config["values"]["cache"]["local_only"] = false.into();
    config["values"]["remote"] = serde_json::json!({"type":"filesystem","path":"redacted"});
    assert_eq!(store_label(&config["values"])?, "local");
    config["values"]["cache"]["remote"] = config["values"]["remote"].clone();
    assert_eq!(store_label(&config["values"])?, "shared");
    Ok(())
}

/// Provenance: SWM-416 review; installed binaries alone do not imply daemon policy.
#[test]
fn kache_daemon_expectation_requires_history_or_explicit_override() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("ledger.json");
    for (seen, policy, expected) in [
        (false, None, 0.),
        (true, None, 1.),
        (false, Some(true), 1.),
        (true, Some(false), 0.),
    ] {
        let ledger = Ledger {
            daemon_seen: seen,
            ..Ledger::default()
        };
        crate::metrics::write_atomic(&path, &serde_json::to_string(&ledger)?)?;
        let mut text = String::new();
        append_for(
            &mut text,
            "laptop",
            &path,
            Some(Path::new("/missing/kache")),
            policy,
            Instant::now() + TIMEOUT,
        );
        assert_eq!(value(&text, "up")?, 0.);
        assert_eq!(value(&text, "daemon_expected")?, expected);
    }
    let (stats, config, explain) = fixtures()?;
    let mut ledger = Ledger::default();
    render(
        &stats,
        &config,
        &explain,
        "luna",
        &mut ledger,
        &mut String::new(),
    )?;
    assert!(ledger.daemon_seen);
    let restored: Ledger = serde_json::from_str(&serde_json::to_string(&ledger)?)?;
    assert!(restored.daemon_seen);
    Ok(())
}

/// Provenance: SWM-416 review; earlier metrics work consumes the shared deadline.
#[test]
fn kache_expired_collection_budget_does_not_start_another_process() -> Result<()> {
    let root = tempfile::tempdir()?;
    let marker = root.path().join("must-not-exist");
    assert!(
        command(
            Path::new("/usr/bin/touch"),
            &[marker.to_str().ok_or_else(unavailable)?],
            Instant::now()
        )
        .is_err()
    );
    assert!(!marker.exists());
    Ok(())
}

/// Provenance: SWM-416 review; real short-window output has lifetime savings but no events.
#[test]
fn kache_counter_fields_share_the_same_event_history_window() -> Result<()> {
    let (_, config, explain) = fixtures()?;
    let stats: Value =
        serde_json::from_str(include_str!("../fixtures/luna-short-window-stats.json"))?;
    let mut text = String::new();
    render(
        &stats,
        &config,
        &explain,
        "luna",
        &mut Ledger::default(),
        &mut text,
    )?;
    assert_eq!(value(&text, "hits_total")?, 0.);
    assert_eq!(value(&text, "misses_total")?, 0.);
    assert_eq!(value(&text, "hit_compile_seconds_total")?, 0.);
    assert_eq!(value(&text, "miss_compile_seconds_total")?, 0.);
    Ok(())
}
