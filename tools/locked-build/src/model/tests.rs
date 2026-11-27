// SPDX-License-Identifier: MIT
//! Deterministic admission oracles against injected clock and host samples.

use super::*;

/// Provenance: SWM-380. Trace: FR-005-AC-4
#[test]
fn stable_boot_identity_prevents_timestamp_drift_from_expiring_live_leases() {
    let mut state = state(Vec::new());
    state.boot_id = Some("kernel-boot-a".into());
    state.boot_time = 100;
    assert!(!state.rebooted(Some("kernel-boot-a"), 112));
    assert!(state.rebooted(Some("kernel-boot-b"), 100));
    state.boot_id = None;
    assert!(!state.rebooted(Some("kernel-boot-a"), 112));
    assert!(!state.rebooted(None, 112));
    assert!(state.rebooted(None, 700));
}

pub(crate) fn policy() -> Config {
    let mut config = Config::default();
    config.host = "test-host".into();
    config.capacity_units = 2;
    config.maximum_units = 4;
    config.memory_per_unit_bytes = 100;
    config.memory_reserve_bytes = 100;
    config.disk_reserve_bytes = 100;
    config.disk_path = PathBuf::from("/");
    config.maximum_load = 10.0;
    config.aging_ms = 100;
    config.hold_ms = 1000;
    config.termination_grace_ms = 100;
    config.memory_admission = crate::config::MemoryAdmissionMode::LiveAvailable;
    config.memory_budget_bytes = None;
    config
}

fn sample() -> HostSample {
    HostSample {
        host: "test-host".into(),
        observed_at_ms: 500,
        memory_available_bytes: 10_000,
        disk_available_bytes: 10_000,
        load_one: 0.0,
    }
}

fn request(sequence: u64, priority: u32, units: u32, time: u64) -> Request {
    Request {
        telemetry: Default::default(),
        client: None,
        client_pid_namespace: None,
        supervision_detail: None,
        trace: Default::default(),
        command_started_clock_ms: None,
        blocked_at_clock_ms: None,
        active_blocked_reasons: Vec::new(),
        measured_memory_bytes: None,
        pid_namespace: None,
        id: RequestId(format!("r{sequence}")),
        sequence,
        owner: "owner".into(),
        ticket: "TEST-1".into(),
        units,
        priority,
        class: "periodic".to_owned(),
        memory_cap_bytes: None,
        memory_reservation_bytes: None,
        memory_limit_key: None,
        memory_cargo_jobs: None,
        memory_rust_test_threads: None,
        memory_parallelism: None,
        caller_cargo_build_jobs: None,
        caller_rust_test_threads: None,
        caller_cargo_incremental: None,
        caller_cargo_target_dir: None,
        lease_record: None,
        enqueued_at_ms: time,
        enqueued_clock_ms: time,
        started_at_ms: None,
        started_clock_ms: None,
        ended_at_ms: None,
        ended_clock_ms: None,
        heartbeat_at_ms: None,
        worker: None,
        worker_generation: 0,
        watchdog: None,
        command_finished: None,
        phase: Phase::Queued,
        stop: None,
        exit_code: None,
        signal: None,
        reason: None,
        command: vec!["/usr/bin/true".into()],
        cwd: PathBuf::from("/"),
    }
}

fn state(requests: Vec<Request>) -> State {
    State {
        min_reader_version: 1,
        schema_version: 1,
        store_id: "store".into(),
        config_path: PathBuf::from("/policy"),
        host: "test-host".into(),
        boot_time: 1,
        boot_id: None,
        next_sequence: 100,
        next_event: 1,
        pruned_events: 0,
        metrics: MetricsState::default(),
        requests,
        events: Vec::new(),
    }
}

/// Trace: FR-005-AC-8
#[test]
fn stable_boot_identity_and_timestamp_jitter_never_expire_healthy_leases() {
    let mut state = state(vec![request(1, 0, 1, 0)]);
    state.requests[0].phase = Phase::Running;
    state.boot_time = 10_000;
    for boot in [0, 10_001, 9_999, 10_090, 9_910] {
        assert!(!state.rebooted(None, boot));
    }
    assert!(state.rebooted(None, 10_600));
    assert!(state.rebooted(None, 9_400));
    state.boot_time = 0;
    assert!(!state.rebooted(None, 10_000));
    state.boot_time = 10_000;
    // Upgrade adopts a UUID without declaring the existing host rebooted.
    assert!(!state.rebooted(Some("boot-a"), 10_600));
    state.boot_id = Some("boot-a".to_owned());
    assert!(!state.rebooted(Some("boot-a"), 10_600));
    assert!(state.rebooted(Some("boot-b"), 10_000));
    assert_eq!(state.requests[0].phase, Phase::Running);
}

/// Trace: FR-005-AC-1, FR-005-AC-8
#[test]
fn titan_memory_backfill_is_bounded_and_never_overcommits() -> Result<()> {
    let mut config = policy();
    config.capacity_units = 8;
    config.memory_admission = MemoryAdmissionMode::LeaseAccounted;
    config.memory_budget_bytes = Some(49_400_000_000);
    let mut requests = vec![
        request(1, 0, 1, 0),
        request(2, 0, 1, 0),
        request(3, 2, 1, 0),
        request(4, 0, 1, 1),
    ];
    for r in &mut requests[..3] {
        r.memory_cap_bytes = Some(24_000_000_000);
    }
    requests[0].phase = Phase::Running;
    requests[1].phase = Phase::Running;
    requests[3].memory_cap_bytes = Some(6_000_000_000);
    let mut state = state(requests);
    // Literal titan figures: 49.4 - 48 = 1.4 GB, so 6 GB cannot fit.
    assert_eq!(
        state.admission_reason(&RequestId("r4".into()), &config, Some(&sample()), 500, 10)?,
        Some("fifo_or_priority_reservation")
    );
    assert_eq!(
        config.memory_budget_bytes.unwrap_or(0) - state.memory_used(&config),
        1_400_000_000
    );
    // A 24 GB lease ends; a 30 GB head cannot fit, but the 6 GB backfill can.
    state.requests[0].phase = Phase::Succeeded;
    state.requests[2].memory_cap_bytes = Some(30_000_000_000);
    assert_eq!(
        state.admission_reason(&RequestId("r3".into()), &config, Some(&sample()), 500, 10)?,
        Some("memory_budget")
    );
    assert_eq!(
        state.admission_reason(&RequestId("r4".into()), &config, Some(&sample()), 500, 10)?,
        None
    );
    assert_eq!(
        state.admission_reason(&RequestId("r4".into()), &config, Some(&sample()), 500, 400)?,
        Some("fifo_or_priority_reservation")
    );
    Ok(())
}

/// Trace: FR-005-AC-1, FR-005-AC-8
#[test]
fn must_fix_always_precedes_aged_block_or_point() -> Result<()> {
    let config = policy();
    for class in ["block", "point"] {
        let mut old = request(1, 2, 1, 0);
        old.class = class.to_owned();
        let mut urgent = request(2, 3, 1, 9_999);
        urgent.class = "must-fix".to_owned();
        let state = state(vec![old, urgent]);
        assert_eq!(state.next(&config, 10_000).map(|r| r.sequence), Some(2));
        assert_eq!(
            state.admission_reason(
                &RequestId("r2".into()),
                &config,
                Some(&sample()),
                500,
                10_000
            )?,
            None
        );
    }
    Ok(())
}

/// Trace: FR-005-AC-8, FR-015-AC-3
#[test]
fn successful_command_with_degraded_supervision_keeps_real_exit_status() -> Result<()> {
    let mut running = request(1, 0, 1, 0);
    running.phase = Phase::Running;
    let mut state = state(vec![running]);
    state.finish(
        &RequestId("r1".into()),
        Completion {
            code: Some(0),
            signal: None,
            reason: Some("supervisor_lost"),
        },
        &policy(),
        500,
        500,
    )?;
    assert_eq!(state.requests[0].phase, Phase::Succeeded);
    assert_eq!(state.requests[0].exit_code, Some(0));
    assert_eq!(
        state.requests[0].reason.as_deref(),
        Some("supervision_degraded")
    );
    assert_eq!(
        state.events[0].trace.diagnostic.code,
        Some(locked_build::wire::Code::SupervisionLost)
    );
    Ok(())
}

/// Trace: FR-015-AC-3
#[test]
fn queue_trace_measures_each_state_and_overlapping_blockers() {
    use locked_build::wire::BlockObservation;
    let mut request = request(1, 0, 1, 1_000);
    request.trace.queue_depth_at_enqueue = 3;
    request.trace.requested_jobs = Some(8);
    request.trace.requested_memory_bytes = Some(48_000_000_000);
    request.observe_blocks(
        vec![
            BlockObservation {
                reason: "memory_budget".into(),
                value: 54.0,
                limit: 49.4,
                seconds: 0.0,
            },
            BlockObservation {
                reason: "units".into(),
                value: 9.0,
                limit: 8.0,
                seconds: 0.0,
            },
        ],
        1_000,
    );
    request.observe_blocks(Vec::new(), 4_000);
    request.phase = Phase::Running;
    request.started_at_ms = Some(4_000);
    request.started_clock_ms = Some(4_000);
    request.trace.running_at_ms = Some(4_500);
    request.command_started_clock_ms = Some(4_500);
    request.ended_at_ms = Some(6_500);
    request.ended_clock_ms = Some(6_500);
    request.memory_cap_bytes = Some(24_000_000_000);
    request.lease_record = Some(LeaseRecord {
        cargo_build_jobs: Some(4),
        ..Default::default()
    });
    let trace = request.trace_snapshot(99_999);
    assert_eq!(
        (
            trace.queued_seconds,
            trace.admitted_seconds,
            trace.running_seconds
        ),
        (3.0, 0.5, 2.0)
    );
    assert_eq!(
        (trace.requested_jobs, trace.granted_jobs),
        (Some(8), Some(4))
    );
    assert_eq!(
        (trace.requested_memory_bytes, trace.granted_memory_bytes),
        (Some(48_000_000_000), Some(24_000_000_000))
    );
    assert_eq!(
        trace
            .blocked_reasons
            .iter()
            .map(|r| r.seconds)
            .collect::<Vec<_>>(),
        [3.0, 3.0]
    );
}

/// Trace: FR-005-AC-8, FR-015-AC-3
#[test]
fn every_terminal_path_has_code_limit_measurement_and_roundtrips() -> Result<()> {
    use locked_build::wire::Code;
    let cases = [
        (
            Some(StopReason::MemoryCap),
            None,
            Some(9),
            None,
            Code::RssCap,
        ),
        (
            Some(StopReason::TimedOut),
            None,
            Some(9),
            None,
            Code::HoldTimeout,
        ),
        (
            Some(StopReason::Cancelled),
            None,
            Some(15),
            None,
            Code::Cancelled,
        ),
        (None, None, Some(15), None, Code::ExternalSignal),
        (None, Some(101), None, None, Code::CommandExit),
        (
            None,
            None,
            None,
            Some("supervisor_lost"),
            Code::SupervisionLost,
        ),
        (
            None,
            None,
            None,
            Some("host_rebooted"),
            Code::SupervisionLost,
        ),
        (
            None,
            Some(0),
            None,
            Some("supervision_degraded"),
            Code::SupervisionLost,
        ),
        (
            None,
            None,
            None,
            Some("memory_cap_unavailable"),
            Code::RefusedCapUnavailable,
        ),
        (None, None, Some(9), Some("rss_cap"), Code::OomKilled),
    ];
    for (stop, code, signal, reason, expected) in cases {
        let mut running = request(1, 0, 1, 10);
        running.phase = Phase::Running;
        running.stop = stop;
        running.started_at_ms = Some(20);
        running.started_clock_ms = Some(20);
        running.memory_cap_bytes = Some(24_000_000_000);
        running.measured_memory_bytes = Some(25_000_000_000);
        running.watchdog = Some(ProcessIdentity {
            pid: 42,
            started_seconds: 7,
        });
        running.lease_record = Some(LeaseRecord {
            memory_enforcement: if expected == Code::OomKilled {
                MemoryEnforcement::SystemdScope
            } else if expected == Code::RefusedCapUnavailable {
                MemoryEnforcement::Refused
            } else {
                MemoryEnforcement::RssWatchdog
            },
            ..Default::default()
        });
        let mut state = state(vec![running]);
        state.finish(
            &RequestId("r1".into()),
            Completion {
                code,
                signal,
                reason,
            },
            &policy(),
            500,
            500,
        )?;
        let diagnostic = &state.events[0].trace.diagnostic;
        assert_eq!(diagnostic.code, Some(expected));
        assert!(
            diagnostic.limit_key.is_some()
                && diagnostic.limit_value.is_some()
                && diagnostic.measured_value.is_some()
        );
        let message = diagnostic.message.as_deref().unwrap_or("");
        let expected_line = match expected {
            Code::RssCap => {
                "locked-build: LB_RSS_CAP request=r1 limit=policy.memory_limit_per_job_bytes*parallelism=24000000000 measured=25000000000 (rss_bytes) killed=pgid:42/true"
            }
            Code::OomKilled => {
                "locked-build: LB_OOM_KILLED request=r1 limit=policy.memory_limit_per_job_bytes*parallelism=24000000000 measured=>=1 (cgroup oom_kill counter) killed=pgid:42/true"
            }
            Code::HoldTimeout => {
                "locked-build: LB_HOLD_TIMEOUT request=r1 limit=policy.hold_ms=1000 measured=480 (elapsed_ms) killed=pgid:42/true"
            }
            Code::Cancelled => {
                "locked-build: LB_CANCELLED request=r1 limit=policy.run_permitted=true measured=false (cancelled by caller) killed=pgid:42/true"
            }
            Code::ExternalSignal => {
                "locked-build: LB_EXTERNAL_SIGNAL request=r1 limit=policy.external_signals=none measured=15 (terminated by signal 15 from outside locked-build) killed=pgid:42/true"
            }
            Code::CommandExit => {
                "locked-build: LB_COMMAND_EXIT request=r1 limit=policy.success_exit_code=0 measured=101 (command exit status) killed=pgid:42/true"
            }
            Code::SupervisionLost if code == Some(0) => {
                "locked-build: LB_SUPERVISION_LOST request=r1 limit=policy.supervision_required=true measured=degraded (supervision degraded; command exited 0) killed=pgid:42/true"
            }
            Code::SupervisionLost => {
                "locked-build: LB_SUPERVISION_LOST request=r1 limit=policy.supervision_required=true measured=false (worker=unavailable watchdog=42; command exit status not observed) killed=pgid:42/true"
            }
            Code::RefusedCapUnavailable => {
                "locked-build: LB_REFUSED_CAP_UNAVAILABLE request=r1 limit=policy.on_missing_cgroup=rss_watchdog measured=unavailable (required memory cap backend) killed=not-started/true"
            }
            _ => {
                return Err(Error::new(
                    ErrorCode::State,
                    "missing hand-written termination oracle",
                ));
            }
        };
        assert_eq!(message, expected_line);
        assert!(
            message.starts_with("locked-build: LB_")
                && message.contains(" limit=")
                && message.contains(" measured=")
                && message.contains(" killed="),
            "{message}"
        );
        assert_eq!(state.requests[0].trace.finished_at_ms, Some(500));
        let bytes = serde_json::to_vec(&state)?;
        let recovered: State = serde_json::from_slice(&bytes)?;
        assert_eq!(
            recovered.events[0].trace.diagnostic.message,
            diagnostic.message
        );
        assert_eq!(recovered.requests[0].trace.diagnostic.code, Some(expected));
    }
    Ok(())
}

#[test]
fn hold_timeout_reason_names_limit_value_and_policy_field() {
    assert_eq!(
        hold_timeout_reason(5_400_000),
        "hold_timeout: command exceeded policy hold_ms=5400000 ms; change the limit in policy hold_ms"
    );
}

/// Trace: FR-005-AC-1, FR-005-AC-2
#[test]
fn equal_priority_is_fifo_and_priority_wins_before_reservation() {
    let config = policy();
    let mut state = state(vec![request(1, 0, 1, 0), request(2, 0, 1, 0)]);
    assert_eq!(
        state.next(&config, 1).map(|request| request.sequence),
        Some(1)
    );
    state.requests.push(request(3, 2, 1, 1));
    assert_eq!(
        state.next(&config, 1).map(|request| request.sequence),
        Some(3)
    );
}

/// Trace: FR-015-AC-3
#[test]
fn queue_wait_event_uses_monotonic_age_after_reboot_recovery() -> Result<()> {
    let config = policy();
    let mut queued = request(1, 0, 1, 1_000);
    queued.enqueued_clock_ms = 20;
    queued.phase = Phase::Running;
    queued.started_at_ms = Some(6_080);
    queued.started_clock_ms = Some(100);
    let mut state = state(vec![queued]);
    state.boot_time = 5;
    state.event(&RequestId("r1".into()), &config, 6_080, 100)?;
    assert_eq!(state.events[0].wait_ms, 80);
    Ok(())
}

/// Trace: FR-015-AC-3
#[test]
fn queue_wait_event_uses_monotonic_age_despite_wall_clock_step() -> Result<()> {
    let config = policy();
    let mut queued = request(1, 0, 1, 6_000);
    queued.enqueued_clock_ms = 100;
    queued.phase = Phase::Running;
    queued.started_at_ms = Some(999_999_999);
    queued.started_clock_ms = Some(250);
    let mut state = state(vec![queued]);
    state.boot_time = 5;
    state.event(&RequestId("r1".into()), &config, 999_999_999, 250)?;
    assert_eq!(state.events[0].wait_ms, 150);
    Ok(())
}

/// Trace: FR-005-AC-1, FR-005-AC-8
#[test]
fn large_old_request_reserves_total_capacity_under_sustained_priority_arrivals() -> Result<()> {
    let mut config = policy();
    config.aging_ms = 60_000;
    let seventy_minutes = 4_200_000;
    let mut running = request(1, 0, 1, 0);
    running.phase = Phase::Running;
    let mut state = state(vec![running, request(2, 0, 2, 0)]);
    for sequence in 3..100 {
        state
            .requests
            .push(request(sequence, 2, 1, seventy_minutes - 1));
    }
    assert_eq!(
        state
            .next(&config, seventy_minutes)
            .map(|request| request.sequence),
        Some(2)
    );
    assert_eq!(
        state.admission_reason(
            &RequestId("r2".into()),
            &config,
            Some(&sample()),
            500,
            seventy_minutes
        )?,
        Some("capacity_occupied")
    );
    assert_eq!(
        state.admission_reason(
            &RequestId("r3".into()),
            &config,
            Some(&sample()),
            500,
            seventy_minutes
        )?,
        Some("fifo_or_priority_reservation")
    );
    state
        .requests
        .get_mut(0)
        .ok_or_else(|| Error::new(ErrorCode::State, "fixture missing"))?
        .phase = Phase::Succeeded;
    assert_eq!(
        state.admission_reason(
            &RequestId("r2".into()),
            &config,
            Some(&sample()),
            500,
            seventy_minutes
        )?,
        None
    );
    Ok(())
}

/// Trace: FR-005-AC-2, FR-005-AC-3
#[test]
fn capacity_shrink_retains_occupancy_and_suspends_infeasible_reservation() -> Result<()> {
    let mut config = policy();
    let mut running = request(1, 0, 2, 0);
    running.phase = Phase::Running;
    let state = state(vec![running, request(2, 0, 2, 0), request(3, 1, 1, 1)]);
    config.capacity_units = 1;
    assert_eq!(state.used(), 2);
    assert_eq!(
        state.next(&config, 1000).map(|request| request.sequence),
        Some(3)
    );
    assert_eq!(
        state.admission_reason(&RequestId("r3".into()), &config, Some(&sample()), 500, 1000)?,
        Some("capacity_occupied")
    );
    config.capacity_units = 2;
    assert_eq!(
        state.next(&config, 1000).map(|request| request.sequence),
        Some(2)
    );
    Ok(())
}

/// Trace: FR-005-AC-3
#[test]
fn missing_stale_wrong_host_and_resource_pressure_refuse_admission() -> Result<()> {
    let config = policy();
    let state = state(vec![request(1, 0, 1, 0)]);
    let id = RequestId("r1".into());
    assert_eq!(
        state.admission_reason(&id, &config, None, 500, 0)?,
        Some("host_sample_unavailable")
    );
    let mut input = sample();
    input.observed_at_ms = 0;
    assert_eq!(
        state.admission_reason(
            &id,
            &config,
            Some(&input),
            crate::config::SAMPLE_MAX_AGE_MS + 1,
            0
        )?,
        Some("host_sample_invalid_or_stale")
    );
    input = sample();
    input.host = "other".into();
    assert_eq!(
        input.block_reason(&config, 500, 0, 1),
        Some("host_sample_invalid_or_stale")
    );
    input = sample();
    input.disk_available_bytes = 99;
    assert_eq!(input.block_reason(&config, 500, 0, 1), Some("disk_reserve"));
    input = sample();
    input.memory_available_bytes = 199;
    assert_eq!(
        input.block_reason(&config, 500, 0, 1),
        Some("memory_reserve")
    );
    input = sample();
    input.load_one = config.maximum_load;
    assert_eq!(input.block_reason(&config, 500, 0, 1), Some("host_load"));
    Ok(())
}

#[test]
fn lease_accounted_admission_uses_caps_without_reading_available_memory() -> Result<()> {
    let mut config = policy();
    config.memory_admission = crate::config::MemoryAdmissionMode::LeaseAccounted;
    config.memory_budget_bytes = Some(1_000);

    let mut running = request(1, 0, 1, 0);
    running.phase = Phase::Running;
    running.memory_cap_bytes = Some(650);
    let mut queued = request(2, 0, 1, 0);
    queued.memory_cap_bytes = Some(350);
    let mut state = state(vec![running, queued]);
    let mut sample = sample();
    sample.memory_available_bytes = 0;

    assert_eq!(
        state.admission_reason(&RequestId("r2".into()), &config, Some(&sample), 500, 500,)?,
        None
    );
    state.find_mut(&RequestId("r2".into()))?.memory_cap_bytes = Some(351);
    assert_eq!(
        state.admission_reason(&RequestId("r2".into()), &config, Some(&sample), 500, 500,)?,
        Some("memory_budget")
    );
    Ok(())
}

/// Trace: FR-005-AC-5, FR-005-AC-7
#[test]
fn terminal_status_and_transition_telemetry_preserve_exit_and_signal() -> Result<()> {
    let config = policy();
    let mut request = request(1, 2, 1, 10);
    request.started_clock_ms = Some(20);
    request.started_at_ms = Some(20);
    request.phase = Phase::Running;
    let mut state = state(vec![request]);
    state.finish(
        &RequestId("r1".into()),
        Completion {
            code: Some(75),
            ..Completion::default()
        },
        &config,
        30,
        30,
    )?;
    let request = state.find(&RequestId("r1".into()))?;
    assert_eq!(request.phase, Phase::Failed);
    assert_eq!(request.exit_code, Some(75));
    let event = state
        .events
        .first()
        .ok_or_else(|| Error::new(ErrorCode::State, "missing event"))?;
    assert_eq!(event.wait_ms, 10);
    assert_eq!(event.hold_ms, Some(10));
    assert_eq!(event.free_units, 2);
    assert_eq!(event.event_id, "store:e1");
    let encoded = serde_json::to_string(event)?;
    assert!(!encoded.contains("/usr/bin/true"));
    Ok(())
}
