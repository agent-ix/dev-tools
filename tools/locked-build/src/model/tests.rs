// SPDX-License-Identifier: MIT
//! Deterministic admission oracles against injected clock and host samples.

use super::*;

pub(crate) fn policy() -> Config {
    Config {
        host: "test-host".into(),
        capacity_units: 2,
        maximum_units: 4,
        memory_per_unit_bytes: 100,
        memory_reserve_bytes: 100,
        disk_reserve_bytes: 100,
        disk_path: PathBuf::from("/"),
        maximum_load: 10.0,
        maximum_priority: 2,
        aging_ms: 100,
        hold_ms: 1000,
        termination_grace_ms: 100,
        sample_max_age_ms: 100,
        heartbeat_max_age_ms: 100,
        poll_ms: 10,
        maximum_requests: 100,
        sample_file: None,
    }
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
        id: RequestId(format!("r{sequence}")),
        sequence,
        owner: "owner".into(),
        ticket: "TEST-1".into(),
        units,
        priority,
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
        schema_version: 1,
        store_id: "store".into(),
        config_path: PathBuf::from("/policy"),
        host: "test-host".into(),
        boot_time: 1,
        next_sequence: 100,
        next_event: 1,
        pruned_events: 0,
        metrics: MetricsState::default(),
        requests,
        events: Vec::new(),
    }
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
    input.observed_at_ms = 399;
    assert_eq!(
        state.admission_reason(&id, &config, Some(&input), 500, 0)?,
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
