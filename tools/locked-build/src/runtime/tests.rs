// SPDX-License-Identifier: MIT
//! Controlled recovery/claim interleavings and generation-fence regressions.

use std::fs::{File, TryLockError};
use std::sync::mpsc;

use super::*;
use crate::config::{Config, HostSample};
use crate::model::ProcessIdentity;

/// Provenance: SWM-374. Trace: FR-011-AC-7
#[test]
fn readonly_target_root_uses_shared_writable_targets() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = enqueue(
        &store,
        "swm-374-target-fixture",
        "SWM-374",
        1,
        0,
        None,
        &["true".into()],
    )?;
    let mut policy = Config::read(&store.config_path)?;
    let readonly = root.path().join("readonly");
    std::fs::create_dir(&readonly)?;
    std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o500))?;
    policy.target_root = Some(readonly.join("targets"));
    let mut request = store.transaction(|state, _| Ok(state.find(&id)?.clone()))?;
    let result = target_directory(&policy, &request, true);
    std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o700))?;
    let target = result?.ok_or_else(|| Error::new(ErrorCode::State, "missing fallback target"))?;
    assert_eq!(
        target.parent(),
        Some(crate::writable::shared_root()?.join("targets").as_path())
    );
    std::fs::write(target.join("test-output"), b"hand-written")?;
    request.lease_record = Some(crate::model::LeaseRecord {
        cargo_target_dir: Some(target.clone()),
        ..Default::default()
    });
    assert_eq!(
        target_directory(&policy, &request, false)?,
        Some(target.clone())
    );
    std::fs::remove_dir_all(target)?;
    Ok(())
}

#[derive(Clone, Copy)]
struct TestClock {
    now_ms: u64,
    clock_ms: u64,
}

impl TestClock {
    fn new() -> Result<Self> {
        Ok(Self {
            now_ms: now_ms()?,
            clock_ms: clock_ms()?,
        })
    }

    fn advance(self, millis: u64) -> Self {
        Self {
            now_ms: self.now_ms.saturating_add(millis),
            clock_ms: self.clock_ms.saturating_add(millis),
        }
    }
}

fn admit_at(
    store: &Store,
    id: &RequestId,
    fence: &WorkerFence,
    clock: TestClock,
) -> Result<Admission> {
    admit_worker_at(
        store,
        id,
        fence,
        &AtomicBool::new(false),
        clock.now_ms,
        clock.clock_ms,
        |config, now| {
            Some(HostSample {
                host: config.host.clone(),
                observed_at_ms: now,
                memory_available_bytes: 1_000_000,
                disk_available_bytes: 1_000_000,
                load_one: 0.0,
            })
        },
    )
}

fn metrics_at_clock(store: &Store, clock: TestClock) -> Result<String> {
    metrics_at(store, clock.now_ms, clock.clock_ms)
}

fn cancel_at_clock(store: &Store, id: &RequestId, clock: TestClock) -> Result<()> {
    cancel_at(store, id, clock.now_ms, clock.clock_ms)
}

fn store(root: &Path) -> Result<Store> {
    let mut policy: Config = serde_json::from_str(include_str!("../../policy.example.json"))?;
    policy.capacity_units = 1;
    policy.memory_per_unit_bytes = 1;
    policy.memory_reserve_bytes = 0;
    policy.disk_reserve_bytes = 0;
    policy.maximum_load = 1_000_000.0;
    policy.memory_admission = crate::config::MemoryAdmissionMode::LiveAvailable;
    policy.memory_budget_bytes = None;
    policy.memory_cap_by_class.clear();
    let sample_path = root.join("sample.json");
    let sample = crate::config::HostSample {
        host: policy.host.clone(),
        observed_at_ms: now_ms()?,
        memory_available_bytes: 1_000_000,
        disk_available_bytes: 1_000_000,
        load_one: 0.0,
    };
    std::fs::write(&sample_path, serde_json::to_vec(&sample)?)?;
    policy.sample_file = Some(sample_path);
    let path = root.join("policy.json");
    std::fs::write(&path, serde_json::to_vec(&policy)?)?;
    Store::open(&root.join("queue"), &path)
}

fn lease_accounted_store(root: &Path, capacity: u32, budget: u64, reserve: u64) -> Result<Store> {
    let initial = store(root)?;
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&initial.config_path)?)?;
    policy.capacity_units = capacity;
    policy.maximum_units = capacity;
    policy.memory_admission = crate::config::MemoryAdmissionMode::LeaseAccounted;
    policy.memory_budget_bytes = Some(budget);
    policy.memory_reserve_bytes = reserve;
    policy.memory_per_job_bytes = 2_500_000_000;
    policy.memory_per_job_by_class.clear();
    policy.memory_cap_by_class.clear();
    policy.cargo_build_jobs = Some(capacity);
    policy.rust_test_threads = None;
    std::fs::write(&initial.config_path, serde_json::to_vec(&policy)?)?;
    Store::open(&initial.path, &initial.config_path)
}

fn queued(store: &Store) -> Result<RequestId> {
    enqueue(
        store,
        "test:team:lead",
        "TEST-1",
        1,
        0,
        None,
        &["/usr/bin/true".into()],
    )
}

/// Trace: FR-005-AC-4
#[test]
fn refused_command_cannot_finish_after_worker_fence_changes() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    store.transaction(|state, config| {
        let identity = ProcessIdentity {
            pid: 41,
            started_seconds: 7,
        };
        let request = state.find_mut(&id)?;
        request.phase = Phase::Running;
        request.worker = Some(identity.clone());
        request.worker_generation = 2;
        request.started_at_ms = Some(1);
        request.started_clock_ms = Some(1);
        let stale_fence = WorkerFence {
            identity,
            generation: 1,
        };

        assert!(finish_refused_command(state, config, &id, &stale_fence).is_err());
        let request = state.find(&id)?;
        assert_eq!(request.phase, Phase::Running);
        assert_eq!(request.reason, None);
        Ok(())
    })
}

/// Trace: FR-011-AC-7
#[test]
fn direct_run_default_class_is_persisted_on_the_request() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    store.transaction(|state, _| {
        assert_eq!(state.find(&id)?.class, "periodic");
        Ok(())
    })
}

fn live_worker_process(id: &RequestId) -> Result<(Child, ProcessIdentity)> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("trap '' TERM; sleep 60")
        .arg("worker")
        .arg(&id.0)
        .process_group(0)
        .spawn()?;
    for _ in 0..50 {
        let system = sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::nothing()
                .with_processes(sysinfo::ProcessRefreshKind::everything()),
        );
        if let Some(process) = system.process(sysinfo::Pid::from_u32(child.id())) {
            let identity = ProcessIdentity {
                pid: child.id(),
                started_seconds: process.start_time(),
            };
            if Processes::read().identity(&identity, id) == IdentityState::Matching {
                return Ok((child, identity));
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    stop_child_group(&mut child)?;
    Err(Error::new(
        ErrorCode::State,
        "test worker process identity was not visible",
    ))
}

fn stop_child_group(child: &mut Child) -> Result<()> {
    let group = i32::try_from(child.id())
        .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(group),
        nix::sys::signal::Signal::SIGKILL,
    );
    let _ = child.wait()?;
    Ok(())
}

/// Trace: FR-015-AC-1
#[test]
fn runtime_admission_and_completion_update_exported_metrics() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    let identity = ProcessIdentity {
        pid: u32::MAX - 1,
        started_seconds: 1,
    };
    let fence = claim_worker(&store, &id, &identity)?
        .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
    assert!(matches!(
        admit_worker(&store, &id, &fence, &AtomicBool::new(false))?,
        Admission::Granted(_)
    ));

    let admitted = metrics(&store)?;
    assert!(admitted.contains("ix_build_slots_busy{host=\"your-build-host\"} 1"));
    assert!(admitted.contains("ix_build_slots_configured{host=\"your-build-host\"} 1"));
    assert!(admitted.contains(
        "ix_build_queue_wait_seconds_count{host=\"your-build-host\",class=\"periodic\"} 1"
    ));
    assert!(
        admitted.contains("ix_build_queue_depth{host=\"your-build-host\",class=\"periodic\"} 0")
    );

    store.transaction(|state, config| {
        state.finish(
            &id,
            Completion {
                code: Some(0),
                ..Completion::default()
            },
            config,
            now_ms()?,
            clock_ms()?,
        )
    })?;
    let completed = metrics(&store)?;
    assert!(completed.contains(
        "ix_build_builds_total{host=\"your-build-host\",class=\"periodic\",outcome=\"ok\"} 1"
    ));
    assert!(completed.contains("ix_build_slots_busy{host=\"your-build-host\"} 0"));
    let state_bytes = std::fs::read_to_string(store.path.join("state.json"))?;
    assert!(!state_bytes.contains("\"metrics\""));
    Ok(())
}

/// Trace: FR-015-AC-11
#[test]
fn runtime_counts_one_blocked_interval_for_multiple_waiters_and_closes_it_on_last_leave()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let config_path = root.path().join("policy.json");
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    policy.memory_reserve_bytes = 2_000_000;
    std::fs::write(&config_path, serde_json::to_vec(&policy)?)?;

    let first = queued(&store)?;
    let second = queued(&store)?;
    let first_fence = claim_worker(
        &store,
        &first,
        &ProcessIdentity {
            pid: u32::MAX - 1,
            started_seconds: 1,
        },
    )?
    .ok_or_else(|| Error::new(ErrorCode::State, "first worker claim missing"))?;
    let second_fence = claim_worker(
        &store,
        &second,
        &ProcessIdentity {
            pid: u32::MAX - 2,
            started_seconds: 1,
        },
    )?
    .ok_or_else(|| Error::new(ErrorCode::State, "second worker claim missing"))?;
    let clock = TestClock::new()?;
    assert!(matches!(
        admit_at(&store, &first, &first_fence, clock)?,
        Admission::Waiting(_)
    ));
    assert!(matches!(
        admit_at(&store, &second, &second_fence, clock)?,
        Admission::Waiting(_)
    ));

    let blocked = metrics_at_clock(&store, clock.advance(1_000))?;
    assert!(blocked.contains("reason=\"memory\""));
    let blocked_value = metric_value(
        &blocked,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(blocked_value > 0.0);

    cancel_at_clock(&store, &first, clock.advance(1_100))?;
    let one_waiter_left = metrics_at_clock(&store, clock.advance(1_200))?;
    let after_first_leave = metric_value(
        &one_waiter_left,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(after_first_leave >= blocked_value);
    cancel_at_clock(&store, &second, clock.advance(1_300))?;
    let closed = metrics_at_clock(&store, clock.advance(1_400))?;
    let closed_value = metric_value(
        &closed,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let later = metrics_at_clock(&store, clock.advance(1_500))?;
    let later_value = metric_value(
        &later,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert_eq!(closed_value, later_value);
    Ok(())
}

/// Trace: FR-015-AC-11
#[test]
fn host_blocked_accumulator_survives_late_polls_recovery_and_interrupts() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let config_path = root.path().join("policy.json");
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    policy.memory_reserve_bytes = 2_000_000;
    std::fs::write(&config_path, serde_json::to_vec(&policy)?)?;
    let first = queued(&store)?;
    let second = queued(&store)?;
    let (mut first_child, first_identity) = live_worker_process(&first)?;
    let (mut second_child, second_identity) = live_worker_process(&second)?;
    let first_fence = claim_worker(&store, &first, &first_identity)?
        .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
    let second_fence = claim_worker(&store, &second, &second_identity)?
        .ok_or_else(|| Error::new(ErrorCode::State, "second worker claim missing"))?;
    assert!(matches!(
        admit_worker(&store, &first, &first_fence, &AtomicBool::new(false))?,
        Admission::Waiting(_)
    ));
    assert!(matches!(
        admit_worker(&store, &second, &second_fence, &AtomicBool::new(false))?,
        Admission::Waiting(_)
    ));

    store.transaction(|state, _| {
        state.find_mut(&first)?.heartbeat_at_ms = Some(now_ms()?.saturating_sub(60_000));
        state.find_mut(&second)?.heartbeat_at_ms = Some(now_ms()?.saturating_sub(60_000));
        Ok(())
    })?;
    let before_lag = metrics(&store)?;
    let value = metric_value(
        &before_lag,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let blocker = File::options()
        .read(true)
        .write(true)
        .open(store.path.join("lock"))?;
    blocker.lock()?;
    let blocked_store = Store {
        path: store.path.clone(),
        config_path: store.config_path.clone(),
    };
    let (sender, receiver) = mpsc::channel();
    let exporter = std::thread::spawn(move || {
        let _ = sender.send(metrics(&blocked_store));
    });
    std::thread::sleep(Duration::from_millis(30));
    blocker.unlock()?;
    let after_lag = receiver
        .recv()
        .map_err(|_| Error::new(ErrorCode::State, "blocked metrics export disappeared"))??;
    exporter
        .join()
        .map_err(|_| Error::new(ErrorCode::State, "metrics exporter panicked"))?;
    let lagged_value = metric_value(
        &after_lag,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(
        lagged_value >= value,
        "counter regressed across stale heartbeat"
    );

    // Recovery of a queued worker and interrupted admission of the selected head
    // must preserve host time while another waiter remains queued.
    stop_child_group(&mut first_child)?;
    recover(&store)?;
    let after_recover = metrics(&store)?;
    let recovered_value = metric_value(
        &after_recover,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(recovered_value >= lagged_value);
    let reopened = Store::open(&store.path, &store.config_path)?;
    let after_reopen = metrics(&reopened)?;
    let reopened_value = metric_value(
        &after_reopen,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(reopened_value >= recovered_value);

    let replacement = claim_worker(
        &reopened,
        &first,
        &ProcessIdentity {
            pid: u32::MAX - 3,
            started_seconds: 1,
        },
    )?
    .ok_or_else(|| Error::new(ErrorCode::State, "recovered first worker claim missing"))?;
    assert!(matches!(
        admit_worker(&reopened, &first, &replacement, &AtomicBool::new(true))?,
        Admission::Stopped
    ));
    let after_interrupt = metrics(&reopened)?;
    let interrupted_value = metric_value(
        &after_interrupt,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(interrupted_value >= recovered_value);
    cancel(&reopened, &second)?;
    stop_child_group(&mut second_child)?;
    Ok(())
}

/// Trace: FR-015-AC-11
#[test]
fn expired_last_waiter_checkpoints_and_persists_without_counter_regression() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let config_path = root.path().join("policy.json");
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    policy.memory_reserve_bytes = 2_000_000;
    std::fs::write(&config_path, serde_json::to_vec(&policy)?)?;
    let id = queued(&store)?;
    let (mut child, identity) = live_worker_process(&id)?;
    let fence = claim_worker(&store, &id, &identity)?
        .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
    assert!(matches!(
        admit_worker(&store, &id, &fence, &AtomicBool::new(false))?,
        Admission::Waiting(_)
    ));
    std::thread::sleep(Duration::from_millis(
        2 * crate::config::POLL_INTERVAL_MS + 50,
    ));
    stop_child_group(&mut child)?;

    let expired = metrics(&store)?;
    let value = metric_value(
        &expired,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(
        value > 0.0,
        "expiry discarded the last valid blocked interval"
    );
    let sidecar: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.path.join("metrics.json"))?)?;
    assert!(sidecar["blocked_millis"]["memory"].as_u64().unwrap_or(0) > 0);
    assert!(sidecar["active_block"].is_null());

    let reopened = Store::open(&store.path, &store.config_path)?;
    let after_reopen = metrics(&reopened)?;
    let reopened_value = metric_value(
        &after_reopen,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(reopened_value >= value);
    cancel(&reopened, &id)?;
    Ok(())
}

/// Trace: FR-015-AC-11
#[test]
fn metrics_exposition_checkpoints_monotonic_and_utc_block_starts() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let config_path = root.path().join("policy.json");
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    policy.memory_reserve_bytes = 2_000_000;
    std::fs::write(&config_path, serde_json::to_vec(&policy)?)?;
    let id = queued(&store)?;
    let fence = claim_worker(
        &store,
        &id,
        &ProcessIdentity {
            pid: u32::MAX - 7,
            started_seconds: 1,
        },
    )?
    .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
    let clock = TestClock::new()?;
    assert!(matches!(
        admit_at(&store, &id, &fence, clock)?,
        Admission::Waiting(_)
    ));

    let first = metrics_at_clock(&store, clock)?;
    let first_value = metric_value(
        &first,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let checkpoint_before: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.path.join("metrics.json"))?)?;
    let clock_before = checkpoint_before["active_block"][1]
        .as_u64()
        .ok_or_else(|| Error::new(ErrorCode::State, "monotonic start missing"))?;
    let utc_before = checkpoint_before["active_started_at_ms"]
        .as_u64()
        .ok_or_else(|| Error::new(ErrorCode::State, "UTC start missing"))?;

    let second = metrics_at_clock(&store, clock.advance(100))?;
    let second_value = metric_value(
        &second,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let checkpoint_after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.path.join("metrics.json"))?)?;
    assert!(second_value >= first_value);
    assert!(
        checkpoint_after["blocked_millis"]["memory"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    assert!(
        checkpoint_after["active_block"][1]
            .as_u64()
            .is_some_and(|after| after > clock_before)
    );
    assert!(
        checkpoint_after["active_started_at_ms"]
            .as_u64()
            .is_some_and(|after| after > utc_before)
    );
    Ok(())
}

/// Trace: FR-015-AC-11
#[test]
fn host_interval_reselects_the_next_queued_reason_after_head_cancellation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let config_path = root.path().join("policy.json");
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    policy.maximum_units = 2;
    policy.memory_per_unit_bytes = 100;
    policy.memory_reserve_bytes = 0;
    policy.disk_reserve_bytes = 2_000_000;
    let sample_path = policy
        .sample_file
        .clone()
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "sample path missing"))?;
    let mut sample: crate::config::HostSample =
        serde_json::from_slice(&std::fs::read(&sample_path)?)?;
    sample.memory_available_bytes = 150;
    sample.disk_available_bytes = 1_000_000;
    std::fs::write(&sample_path, serde_json::to_vec(&sample)?)?;
    std::fs::write(&config_path, serde_json::to_vec(&policy)?)?;

    let first = enqueue(
        &store,
        "test:team:lead",
        "TEST-LARGE",
        2,
        0,
        None,
        &["/usr/bin/true".into()],
    )?;
    let second = enqueue(
        &store,
        "test:team:lead",
        "TEST-SMALL",
        1,
        0,
        None,
        &["/usr/bin/true".into()],
    )?;
    for (id, pid) in [(&first, u32::MAX - 5), (&second, u32::MAX - 6)] {
        let fence = claim_worker(
            &store,
            id,
            &ProcessIdentity {
                pid,
                started_seconds: 1,
            },
        )?
        .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
        assert!(matches!(
            admit_worker(&store, id, &fence, &AtomicBool::new(false))?,
            Admission::Waiting(_)
        ));
    }
    let before = metrics(&store)?;
    cancel(&store, &first)?;
    let after = metrics(&store)?;
    let memory_before = metric_value(
        &before,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let memory_after = metric_value(
        &after,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let disk_after = metric_value(
        &after,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"target_disk\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "target disk series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(memory_after >= memory_before);
    assert!(
        disk_after > 0.0,
        "next queued request's reason was not selected"
    );
    std::thread::sleep(Duration::from_millis(20));
    let later = metrics(&store)?;
    let memory_later = metric_value(
        &later,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let disk_later = metric_value(
        &later,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"target_disk\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "target disk series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert_eq!(memory_later, memory_after);
    assert!(disk_later >= disk_after);
    cancel(&store, &second)?;
    Ok(())
}

/// Trace: FR-015-AC-11
#[test]
fn hold_expiry_of_running_request_does_not_reset_queued_host_interval() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let config_path = root.path().join("policy.json");
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    policy.memory_reserve_bytes = 2_000_000;
    std::fs::write(&config_path, serde_json::to_vec(&policy)?)?;
    let running = queued(&store)?;
    let queued_id = queued(&store)?;
    store.transaction(|state, config| {
        let request = state.find_mut(&running)?;
        request.phase = Phase::Running;
        request.started_clock_ms = Some(clock_ms()?.saturating_sub(config.hold_ms));
        request.started_at_ms = Some(now_ms()?);
        request.worker = Some(ProcessIdentity {
            pid: u32::MAX - 4,
            started_seconds: 1,
        });
        request.worker_generation = 1;
        request.stop = Some(crate::model::StopReason::TimedOut);
        state
            .metrics
            .observe_block(Some("memory_reserve"), now_ms()?, clock_ms()?)
    })?;
    let before = metrics(&store)?;
    let before_value = metric_value(
        &before,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    store.transaction(|state, config| {
        state.finish(
            &running,
            Completion::default(),
            config,
            now_ms()?,
            clock_ms()?,
        )
    })?;
    let after = metrics(&store)?;
    let after_value = metric_value(
        &after,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(after_value >= before_value);
    assert_eq!(state_phase(&store, &queued_id)?, Phase::Queued);
    cancel(&store, &queued_id)?;
    Ok(())
}

fn state_phase(store: &Store, id: &RequestId) -> Result<Phase> {
    store.transaction(|state, _| Ok(state.find(id)?.phase))
}

fn metric_value<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines().find_map(|line| line.strip_prefix(prefix))
}

/// Trace: FR-015-AC-1
#[test]
fn metrics_open_refuses_a_missing_state_path_without_creating_it() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = root.path().join("policy.json");
    let policy = Config {
        host: "host".to_owned(),
        disk_path: root.path().to_owned(),
        memory_admission: crate::config::MemoryAdmissionMode::LiveAvailable,
        memory_per_unit_bytes: 1,
        ..Config::default()
    };
    std::fs::write(&config, serde_json::to_vec(&policy)?)?;
    let missing = root.path().join("not-created");
    assert!(Store::open_existing(&missing, &config).is_err());
    assert!(!missing.exists());
    Ok(())
}

#[test]
fn lease_environment_uses_policy_values_and_scales_jobs_by_active_units() -> Result<()> {
    let mut policy: Config = serde_json::from_str(include_str!("../../policy.example.json"))?;
    policy.cargo_build_jobs = Some(8);
    policy.rust_test_threads = Some(3);
    policy.cargo_incremental = Some(false);

    let mut command = Command::new("/usr/bin/env");
    command
        .env_remove("CARGO_BUILD_JOBS")
        .env_remove("RUST_TEST_THREADS")
        .env_remove("CARGO_INCREMENTAL")
        .env_remove("LOCKED_BUILD_LEASE");
    let mut arguments = Vec::new();
    let prepared = prepare_build_environment(&policy, &mut arguments, None, None, 2);
    if let Some(value) = prepared.cargo_build_jobs_env {
        command.env("CARGO_BUILD_JOBS", value);
    }
    if let Some(value) = prepared.rust_test_threads_env {
        command.env("RUST_TEST_THREADS", value);
    }
    apply_incremental_environment(&mut command, &policy, None);
    let output = command.output()?;
    assert!(output.status.success());
    let environment = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut actual: Vec<_> = environment
        .lines()
        .filter(|line| {
            line.starts_with("CARGO_BUILD_JOBS=")
                || line.starts_with("RUST_TEST_THREADS=")
                || line.starts_with("CARGO_INCREMENTAL=")
                || line.starts_with("LOCKED_BUILD_LEASE=")
        })
        .collect();
    actual.sort_unstable();
    assert_eq!(
        actual,
        [
            "CARGO_BUILD_JOBS=4",
            "CARGO_INCREMENTAL=0",
            "LOCKED_BUILD_LEASE=1",
            "RUST_TEST_THREADS=3"
        ]
    );
    assert_eq!(prepared.record.cargo_build_jobs, Some(4));
    assert_eq!(prepared.record.rust_test_threads, Some(3));
    Ok(())
}

#[test]
fn direct_test_binary_thread_limit_is_clamped_without_separator() {
    let mut arguments = vec!["--test-threads=20".to_owned()];
    let limited = clamp_test_threads(&mut arguments, Some(4));
    assert!(limited.is_some_and(|limit| limit.clamped));
    assert_eq!(arguments, ["--test-threads=4"]);
}

#[test]
fn caller_environment_overrides_incremental_and_policy_target_values() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    let request = store.transaction(|state, _| Ok(state.find(&id)?.clone()))?;
    let mut policy: Config = serde_json::from_str(include_str!("../../policy.example.json"))?;
    policy.cargo_incremental = None;
    let policy_target_root = root.path().join("policy-targets");
    policy.target_root = Some(policy_target_root.clone());
    let mut request = request;
    request.caller_cargo_incremental = Some("1".to_owned());
    request.caller_cargo_target_dir = Some("/caller/target".to_owned());
    let mut command = Command::new("/usr/bin/env");
    command
        .env_remove("CARGO_INCREMENTAL")
        .env_remove("CARGO_TARGET_DIR");
    let (incremental, incremental_overwrote) = apply_incremental_environment(
        &mut command,
        &policy,
        request.caller_cargo_incremental.as_deref(),
    );
    let (target, target_overwrote) = apply_target_environment(&mut command, &policy, &request)?;
    let output = command.output()?;
    assert!(output.status.success());
    let environment = String::from_utf8_lossy(&output.stdout);
    assert!(
        environment
            .lines()
            .any(|line| line == "CARGO_INCREMENTAL=1")
    );
    assert!(
        environment
            .lines()
            .any(|line| line == "CARGO_TARGET_DIR=/caller/target")
    );
    assert_eq!(incremental.as_deref(), Some("1"));
    assert!(!incremental_overwrote);
    assert_eq!(
        target.as_deref(),
        Some(std::path::Path::new("/caller/target"))
    );
    assert!(!target_overwrote);
    assert!(!policy_target_root.exists());

    let mut policy_override = policy;
    policy_override.cargo_incremental = Some(false);
    let mut command = Command::new("/usr/bin/true");
    let (effective, overwritten) =
        apply_incremental_environment(&mut command, &policy_override, Some("1"));
    assert_eq!(effective.as_deref(), Some("0"));
    assert!(overwritten);
    Ok(())
}

#[test]
fn explicit_cargo_limits_win_under_pressure_and_are_recorded() -> Result<()> {
    let mut policy: Config = serde_json::from_str(include_str!("../../policy.example.json"))?;
    policy.cargo_build_jobs = Some(16);
    policy.rust_test_threads = Some(8);
    let mut arguments = [
        "test".to_owned(),
        "-j".to_owned(),
        "4".to_owned(),
        "--".to_owned(),
        "--test-threads=4".to_owned(),
    ];

    let prepared = prepare_build_environment(&policy, &mut arguments, Some("1"), Some("1"), 16);

    assert_eq!(arguments, ["test", "-j", "4", "--", "--test-threads=4"]);
    assert_eq!(prepared.cargo_build_jobs_env.as_deref(), Some("4"));
    assert_eq!(prepared.rust_test_threads_env.as_deref(), Some("4"));
    assert_eq!(prepared.record.cargo_build_jobs, Some(4));
    assert!(!prepared.record.cargo_build_jobs_clamped);
    assert_eq!(prepared.record.rust_test_threads, Some(4));
    assert!(!prepared.record.rust_test_threads_clamped);
    Ok(())
}

#[test]
fn explicit_cargo_env_values_are_clamped_to_policy_maximum() -> Result<()> {
    let mut policy: Config = serde_json::from_str(include_str!("../../policy.example.json"))?;
    policy.cargo_build_jobs = Some(8);
    policy.rust_test_threads = Some(4);
    let mut arguments = Vec::new();

    let prepared = prepare_build_environment(&policy, &mut arguments, Some("12"), Some("9"), 8);

    assert_eq!(prepared.cargo_build_jobs_env.as_deref(), Some("8"));
    assert_eq!(prepared.rust_test_threads_env.as_deref(), Some("4"));
    assert_eq!(prepared.record.cargo_build_jobs, Some(8));
    assert!(prepared.record.cargo_build_jobs_clamped);
    assert_eq!(prepared.record.rust_test_threads, Some(4));
    assert!(prepared.record.rust_test_threads_clamped);
    Ok(())
}

#[test]
fn invalid_cargo_job_limits_are_rejected_in_arguments_and_environment() {
    for arguments in [
        vec![
            "cargo".to_owned(),
            "test".to_owned(),
            "-j".to_owned(),
            "-1".to_owned(),
        ],
        vec![
            "cargo".to_owned(),
            "test".to_owned(),
            "--jobs=-1".to_owned(),
        ],
        vec![
            "cargo".to_owned(),
            "test".to_owned(),
            "-j".to_owned(),
            "default".to_owned(),
        ],
        vec!["cargo".to_owned(), "test".to_owned(), "-j".to_owned()],
    ] {
        assert!(validate_parallelism_input(&arguments, None, None).is_err());
    }
    assert!(
        validate_parallelism_input(&["cargo".into(), "test".into()], Some("-1"), None).is_err()
    );
    assert!(
        validate_parallelism_input(&["cargo".into(), "test".into()], Some("default"), None)
            .is_err()
    );
    assert!(validate_parallelism_input(&["cargo".into(), "test".into()], Some("8"), None).is_ok());
}

#[test]
fn memory_charge_uses_test_threads_and_non_cargo_environment_is_capped_by_units() -> Result<()> {
    let mut policy: Config = serde_json::from_str(include_str!("../../policy.example.json"))?;
    policy.memory_admission = crate::config::MemoryAdmissionMode::LeaseAccounted;
    policy.memory_budget_bytes = Some(100_000_000_000);
    policy.cargo_build_jobs = Some(16);
    policy.rust_test_threads = Some(16);
    policy.memory_per_job_bytes = 2_500_000_000;

    let cargo_command = [
        "cargo".into(),
        "test".into(),
        "-j4".into(),
        "--".into(),
        "--test-threads=8".into(),
    ];
    validate_parallelism_input(&cargo_command, None, None)?;
    let (jobs, threads, parallelism) =
        resolve_build_parallelism(&policy, &cargo_command, None, None, 1, 1);
    assert_eq!(jobs, Some(4));
    assert_eq!(threads, Some(8));
    assert_eq!(parallelism, 8);
    assert_eq!(
        estimate_memory_lease(&policy, "periodic", parallelism)?,
        20_000_000_000
    );

    let make_command = ["make".into(), "-j8".into()];
    let (jobs, threads, parallelism) =
        resolve_build_parallelism(&policy, &make_command, Some("16"), Some("12"), 1, 1);
    assert_eq!(jobs, None);
    assert_eq!(threads, Some(1));
    assert_eq!(parallelism, 1);
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    let mut request = store.transaction(|state, _| Ok(state.find(&id)?.clone()))?;
    request.command = make_command.to_vec();
    request.caller_cargo_build_jobs = Some("16".to_owned());
    request.caller_rust_test_threads = Some("12".to_owned());
    request.memory_parallelism = Some(parallelism);
    request.memory_rust_test_threads = threads;
    request.memory_cargo_jobs = jobs;
    let mut arguments = request.command[1..].to_vec();
    let environment = prepare_request_environment(&policy, &request, &mut arguments, 1);
    assert_eq!(environment.cargo_build_jobs_env.as_deref(), Some("1"));
    assert_eq!(environment.rust_test_threads_env.as_deref(), Some("1"));
    Ok(())
}

#[test]
fn named_priority_uses_policy_mapping_and_rejects_unconfigured_classes() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&store.config_path)?)?;
    policy.priority_classes.insert("must-fix".to_owned(), 2);
    std::fs::write(&store.config_path, serde_json::to_vec(&policy)?)?;

    assert_eq!(priority_for_class(&store, "must-fix")?, 2);
    assert!(priority_for_class(&store, "missing").is_err());
    Ok(())
}

#[test]
fn new_requests_use_per_class_memory_cap_overrides_and_default_cap() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&store.config_path)?)?;
    policy.memory_per_job_bytes = 512;
    policy.memory_admission = crate::config::MemoryAdmissionMode::LeaseAccounted;
    policy.memory_budget_bytes = Some(2_000);
    policy
        .memory_per_job_by_class
        .insert("must-fix".to_owned(), 768);
    policy.priority_classes.insert("must-fix".to_owned(), 2);
    std::fs::write(&store.config_path, serde_json::to_vec(&policy)?)?;

    let class_id = enqueue(
        &store,
        "test:team:lead",
        "TEST-MEMORY-CLASS",
        1,
        2,
        Some("must-fix"),
        &["/usr/bin/true".into()],
    )?;
    let default_id = enqueue(
        &store,
        "test:team:lead",
        "TEST-MEMORY-DEFAULT",
        1,
        2,
        None,
        &["/usr/bin/true".into()],
    )?;
    store.transaction(|state, _| {
        assert_eq!(state.find(&class_id)?.memory_cap_bytes, Some(768));
        assert_eq!(state.find(&default_id)?.memory_cap_bytes, Some(512));
        assert_eq!(state.find(&default_id)?.class, "periodic");
        Ok(())
    })?;
    Ok(())
}

/// Trace: FR-015-AC-8
#[test]
fn lease_accounting_charges_effective_jobs_and_rejects_a_lease_larger_than_budget() -> Result<()> {
    const GIB: u64 = 1_000_000_000;
    let root = tempfile::tempdir()?;
    let store = lease_accounted_store(root.path(), 33, 54 * GIB, 8 * GIB)?;
    let one_job = enqueue(
        &store,
        "test:team:lead",
        "TEST-MEMORY-ONE",
        1,
        0,
        None,
        &["cargo".into(), "build".into(), "-j1".into()],
    )?;
    let eight_jobs = enqueue(
        &store,
        "test:team:lead",
        "TEST-MEMORY-EIGHT",
        1,
        0,
        None,
        &["cargo".into(), "build".into(), "-j8".into()],
    )?;
    let eight_job_lease = store.transaction(|state, _| {
        assert_eq!(state.find(&one_job)?.memory_cap_bytes, Some(2_500_000_000));
        assert_eq!(
            state.find(&eight_jobs)?.memory_cap_bytes,
            Some(20_000_000_000)
        );
        Ok(state.find(&eight_jobs)?.memory_cap_bytes)
    })?;
    let launch = lease_command_spec_for_enforcement(
        LauncherPlatform::Linux,
        "cargo",
        &[],
        eight_job_lease,
        MemoryEnforcement::SystemdScope,
    );
    assert!(
        launch
            .arguments
            .iter()
            .any(|argument| argument == "MemoryMax=20000000000")
    );
    let too_large = enqueue(
        &store,
        "test:team:lead",
        "TEST-MEMORY-TOO-LARGE",
        1,
        0,
        None,
        &["cargo".into(), "build".into(), "-j30".into()],
    );
    assert!(too_large.is_err_and(|error| error.to_string().contains("exceeds memory budget")));
    Ok(())
}

/// Trace: FR-015-AC-8
#[test]
fn start_environment_uses_the_exact_job_count_reserved_by_its_memory_cap() -> Result<()> {
    const GIB: u64 = 1_000_000_000;
    let root = tempfile::tempdir()?;
    let store = lease_accounted_store(root.path(), 16, 54 * GIB, 8 * GIB)?;
    let high_parallelism = enqueue(
        &store,
        "test:team:lead",
        "TEST-MEMORY-JOBS-HIGH",
        1,
        0,
        None,
        &["cargo".into(), "build".into()],
    )?;
    store.transaction(|state, _| {
        assert_eq!(state.find(&high_parallelism)?.memory_cargo_jobs, Some(16));
        assert_eq!(
            state.find(&high_parallelism)?.memory_cap_bytes,
            Some(40 * GIB)
        );
        Ok(())
    })?;

    for index in 0..7_u32 {
        let id = enqueue(
            &store,
            "test:team:lead",
            &format!("TEST-MEMORY-PRESSURE-{index}"),
            1,
            0,
            None,
            &["/usr/bin/true".into()],
        )?;
        let _fence = claim_worker(
            &store,
            &id,
            &ProcessIdentity {
                pid: u32::MAX - index,
                started_seconds: 1,
            },
        )?
        .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
        store.transaction(|state, _| {
            let request = state.find_mut(&id)?;
            request.phase = Phase::Running;
            request.started_at_ms = Some(now_ms()?);
            request.started_clock_ms = Some(clock_ms()?);
            Ok(())
        })?;
    }
    let low_parallelism = enqueue(
        &store,
        "test:team:lead",
        "TEST-MEMORY-JOBS-LOW",
        1,
        0,
        None,
        &["cargo".into(), "build".into()],
    )?;
    let low_request = store.transaction(|state, _| {
        let request = state.find(&low_parallelism)?.clone();
        assert_eq!(request.memory_cargo_jobs, Some(2));
        assert_eq!(request.memory_cap_bytes, Some(5 * GIB));
        Ok(request)
    })?;
    let mut start_policy: Config = serde_json::from_slice(&std::fs::read(&store.config_path)?)?;
    start_policy.cargo_build_jobs = Some(16);
    let mut high_arguments = vec!["build".to_owned()];
    let high_environment = prepare_request_environment(
        &start_policy,
        &store.transaction(|state, _| Ok(state.find(&high_parallelism)?.clone()))?,
        &mut high_arguments,
        16,
    );
    let mut low_arguments = vec!["build".to_owned()];
    let low_environment =
        prepare_request_environment(&start_policy, &low_request, &mut low_arguments, 1);
    assert_eq!(high_environment.record.cargo_build_jobs, Some(16));
    assert_eq!(low_environment.record.cargo_build_jobs, Some(2));
    assert_eq!(
        u64::from(high_environment.record.cargo_build_jobs.unwrap_or_default()) * 2_500_000_000,
        40 * GIB
    );
    assert_eq!(
        u64::from(low_environment.record.cargo_build_jobs.unwrap_or_default()) * 2_500_000_000,
        5 * GIB
    );
    Ok(())
}

/// Trace: FR-015-AC-8
#[test]
fn memory_budget_admits_exactly_twenty_one_one_job_leases_concurrently() -> Result<()> {
    use std::sync::{Arc, Barrier};

    const GIB: u64 = 1_000_000_000;
    let root = tempfile::tempdir()?;
    let initial = lease_accounted_store(root.path(), 33, 54 * GIB, 8 * GIB)?;
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&initial.config_path)?)?;
    policy.sample_file = None;
    std::fs::write(&initial.config_path, serde_json::to_vec(&policy)?)?;
    let store = Store::open(&initial.path, &initial.config_path)?;
    let mut claims = Vec::new();
    for sequence in 0..33_u32 {
        let id = enqueue(
            &store,
            "test:team:lead",
            &format!("TEST-MEMORY-{sequence}"),
            1,
            0,
            None,
            &["cargo".into(), "build".into(), "-j1".into()],
        )?;
        let identity = ProcessIdentity {
            pid: u32::MAX - sequence,
            started_seconds: 1,
        };
        let fence = claim_worker(&store, &id, &identity)?
            .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
        claims.push((id, fence));
    }

    let claims = Arc::new(claims);
    let barrier = Arc::new(Barrier::new(claims.len()));
    let mut threads = Vec::new();
    for index in 0..claims.len() {
        let barrier = barrier.clone();
        let claims = claims.clone();
        let path = store.path.clone();
        let config_path = store.config_path.clone();
        threads.push(std::thread::spawn(move || -> Result<()> {
            let store = Store::open(&path, &config_path)?;
            barrier.wait();
            let (id, fence) = &claims[index];
            let _ = admit_worker(&store, id, fence, &AtomicBool::new(false))?;
            Ok(())
        }));
    }
    for thread in threads {
        thread
            .join()
            .map_err(|_| Error::new(ErrorCode::State, "concurrent admission thread panicked"))??;
    }
    for (id, fence) in claims.iter() {
        let _ = admit_worker(&store, id, fence, &AtomicBool::new(false))?;
    }
    store.transaction(|state, config| {
        let now = now_ms()?;
        let clock = clock_ms()?;
        let sample = config.sample(now).ok();
        let queued_head = state
            .requests
            .iter()
            .find(|request| request.phase == Phase::Queued);
        let head_reason = queued_head
            .map(|request| state.admission_reason(&request.id, config, sample.as_ref(), now, clock))
            .transpose()?;
        let running = state
            .requests
            .iter()
            .filter(|request| request.phase == Phase::Running)
            .count();
        let sum = state
            .requests
            .iter()
            .filter(|request| request.phase == Phase::Running)
            .map(|request| request.memory_cap_bytes.unwrap_or(0))
            .sum::<u64>();
        assert_eq!(
            running,
            21,
            "running={running}, sum={sum}, budget={:?}, capacity={}, used={}, head_reason={head_reason:?}, queued_caps={:?}",
            config.memory_budget_bytes,
            config.capacity_units,
            state.used(),
            state
                .requests
                .iter()
                .filter(|request| request.phase == Phase::Queued)
                .map(|request| request.memory_cap_bytes)
                .collect::<Vec<_>>()
        );
        assert_eq!(sum, 52_500_000_000);
        assert!(sum <= 54 * GIB);
        Ok(())
    })?;
    Ok(())
}

/// Trace: FR-015-AC-8
#[test]
fn memory_budget_survives_reopen_and_releases_on_completion_and_cancel() -> Result<()> {
    const GIB: u64 = 1_000_000_000;
    let root = tempfile::tempdir()?;
    let store = lease_accounted_store(root.path(), 3, 5 * GIB, 0)?;
    let mut requests = Vec::new();
    let mut fences = Vec::new();
    for sequence in 0..4_u32 {
        let id = enqueue(
            &store,
            "test:team:lead",
            &format!("TEST-MEMORY-RELEASE-{sequence}"),
            1,
            0,
            None,
            &["cargo".into(), "build".into(), "-j1".into()],
        )?;
        let fence = claim_worker(
            &store,
            &id,
            &ProcessIdentity {
                pid: u32::MAX - sequence,
                started_seconds: 1,
            },
        )?
        .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
        requests.push(id);
        fences.push(fence);
    }
    let clock = TestClock::new()?;
    assert!(matches!(
        admit_at(&store, &requests[0], &fences[0], clock)?,
        Admission::Granted(_)
    ));
    assert!(matches!(
        admit_at(&store, &requests[1], &fences[1], clock)?,
        Admission::Granted(_)
    ));
    assert!(matches!(
        admit_at(&store, &requests[2], &fences[2], clock)?,
        Admission::Waiting(_)
    ));

    let reopened = Store::open(&store.path, &store.config_path)?;
    assert!(matches!(
        admit_at(&reopened, &requests[2], &fences[2], clock)?,
        Admission::Waiting(_)
    ));
    reopened.transaction_with_clock(
        || Ok((clock.now_ms, clock.clock_ms)),
        |state, config, _, _| {
            state.find_mut(&requests[0])?.stop = Some(StopReason::Cancelled);
            state.finish(
                &requests[0],
                Completion::default(),
                config,
                clock.now_ms,
                clock.clock_ms,
            )
        },
    )?;
    assert!(matches!(
        admit_at(&reopened, &requests[2], &fences[2], clock)?,
        Admission::Granted(_)
    ));
    assert!(matches!(
        admit_at(&reopened, &requests[3], &fences[3], clock)?,
        Admission::Waiting(_)
    ));
    reopened.transaction_with_clock(
        || Ok((clock.now_ms, clock.clock_ms)),
        |state, config, _, _| {
            state.finish(
                &requests[1],
                Completion {
                    code: Some(0),
                    ..Completion::default()
                },
                config,
                clock.now_ms,
                clock.clock_ms,
            )
        },
    )?;
    assert!(matches!(
        admit_at(&reopened, &requests[3], &fences[3], clock)?,
        Admission::Granted(_)
    ));
    Ok(())
}

#[test]
fn lease_launcher_builds_systemd_argv_without_spawning_a_cgroup() {
    let spec = lease_command_spec(
        LauncherPlatform::Linux,
        "/usr/bin/cargo",
        &[
            "test".to_owned(),
            "-p".to_owned(),
            "locked-build".to_owned(),
        ],
        Some(4_294_967_296),
    );
    let actual: Vec<_> = spec
        .arguments
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    assert_eq!(spec.program, "systemd-run");
    assert_eq!(
        actual,
        [
            "--user",
            "--scope",
            "--quiet",
            "-p",
            "MemoryMax=4294967296",
            "-p",
            "MemorySwapMax=0",
            "--",
            "/usr/bin/cargo",
            "test",
            "-p",
            "locked-build"
        ]
    );
}

#[test]
fn non_linux_lease_launcher_uses_direct_command_for_watchdog_fallback() {
    let spec = lease_command_spec(
        LauncherPlatform::Other,
        "/usr/bin/cargo",
        &["check".to_owned()],
        Some(1_024),
    );
    assert_eq!(spec.program, "/usr/bin/cargo");
    assert_eq!(spec.arguments, ["check"]);
}

#[test]
fn failed_systemd_launcher_obeys_missing_cgroup_policy_and_records_choice() -> Result<()> {
    let refused = select_memory_enforcement(
        LauncherPlatform::Linux,
        Some(4_096),
        crate::config::OnMissingCgroup::Refuse,
        |_| false,
    );
    assert_eq!(refused, MemoryEnforcement::Refused);

    let (fallback, fallback_spec) = select_lease_launch(
        LauncherPlatform::Linux,
        "/usr/bin/true",
        &[],
        Some(4_096),
        crate::config::OnMissingCgroup::RssWatchdog,
        |_| false,
    );
    assert_eq!(fallback, MemoryEnforcement::RssWatchdog);
    assert_eq!(fallback_spec.program, "/usr/bin/true");
    assert!(
        Command::new(fallback_spec.program)
            .args(fallback_spec.arguments)
            .status()?
            .success()
    );
    let recorded = LeaseRecord {
        memory_enforcement: fallback,
        ..LeaseRecord::default()
    };
    assert_eq!(
        serde_json::to_value(recorded)?["memory_enforcement"],
        "rss_watchdog"
    );
    assert_eq!(cgroup_cap_kill_reason(Some(fallback), false), None);
    assert_eq!(
        cgroup_cap_kill_reason(Some(MemoryEnforcement::SystemdScope), true),
        Some("rss_cap")
    );
    assert_eq!(
        cgroup_cap_kill_reason(Some(MemoryEnforcement::SystemdScope), false),
        None
    );

    let scoped = select_memory_enforcement(
        LauncherPlatform::Linux,
        Some(4_096),
        crate::config::OnMissingCgroup::Refuse,
        |_| true,
    );
    assert_eq!(scoped, MemoryEnforcement::SystemdScope);
    Ok(())
}

#[test]
fn systemd_scope_probe_requires_delegated_memory_controller() -> Result<()> {
    let root = tempfile::tempdir()?;
    let parent = root.path().join("user.slice");
    let child = parent.join("session.scope");
    std::fs::create_dir_all(&child)?;
    std::fs::write(parent.join("cgroup.subtree_control"), "cpu memory\n")?;
    std::fs::write(child.join("memory.max"), "max\n")?;
    assert!(memory_controller_delegated_at(
        root.path(),
        "user.slice/session.scope"
    ));

    std::fs::write(parent.join("cgroup.subtree_control"), "cpu\n")?;
    assert!(!memory_controller_delegated_at(
        root.path(),
        "user.slice/session.scope"
    ));
    std::fs::write(parent.join("cgroup.subtree_control"), "memory\n")?;
    std::fs::remove_file(child.join("memory.max"))?;
    assert!(!memory_controller_delegated_at(
        root.path(),
        "user.slice/session.scope"
    ));
    assert!(!memory_controller_delegated_at(root.path(), "../escape"));
    Ok(())
}

#[test]
fn supervise_loop_uses_persisted_scope_and_injected_scope_kill() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    let stale_request = store.transaction(|state, _| Ok(state.find(&id)?.clone()))?;
    let mut command = Command::new("/bin/sh")
        .arg("-c")
        .arg("kill -KILL $$")
        .process_group(0)
        .spawn()?;
    let command_pid = command.id();
    let mut outcome = None;
    for _ in 0..50 {
        outcome = command.try_wait()?;
        if outcome.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(outcome.is_some(), "injected command must exit by signal");
    let watchdog = ProcessIdentity {
        pid: command_pid,
        started_seconds: 1,
    };
    let worker = ProcessIdentity {
        pid: u32::MAX - 1,
        started_seconds: 1,
    };
    let fence = claim_worker(&store, &id, &worker)?
        .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
    store.transaction(|state, _| {
        let request = state.find_mut(&id)?;
        request.phase = Phase::Running;
        request.started_at_ms = Some(now_ms()?);
        request.started_clock_ms = Some(clock_ms()?);
        request.watchdog = Some(watchdog.clone());
        request.memory_cap_bytes = Some(1_000_000);
        request.lease_record = Some(LeaseRecord {
            memory_enforcement: MemoryEnforcement::SystemdScope,
            ..LeaseRecord::default()
        });
        Ok(())
    })?;
    assert!(stale_request.lease_record.is_none());
    let events = root.path().join("memory.events");
    std::fs::write(&events, "low 0\nhigh 0\nmax 1\noom 1\noom_kill 1\n")?;
    let mut watchdog_child = Command::new("/usr/bin/true").spawn()?;
    supervise_started_command(
        &store,
        &stale_request,
        &fence,
        &watchdog,
        &mut watchdog_child,
        &AtomicBool::new(false),
        command,
        |evidence, _processes, _group| {
            evidence.observe_file(&events);
            let _ = std::fs::remove_file(&events);
        },
    )?;
    let finished = store.transaction(|state, _| Ok(state.find(&id)?.clone()))?;
    assert_eq!(finished.reason.as_deref(), Some("rss_cap"));
    assert_eq!(finished.signal, Some(9));
    Ok(())
}

#[test]
fn hanging_systemd_probe_is_killed_at_its_timeout() {
    let started = std::time::Instant::now();
    let available = run_probe_with_timeout(std::time::Duration::from_millis(50), || {
        Command::new("/bin/sleep").arg("10").spawn()
    });

    assert!(!available);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn target_lifecycle_assigns_owner_directory_and_deletes_oversized_target() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&store.config_path)?)?;
    let target_root = root.path().join("targets");
    policy.target_root = Some(target_root.clone());
    policy.max_bytes = Some(3);
    std::fs::write(&store.config_path, serde_json::to_vec(&policy)?)?;

    let id = queued(&store)?;
    clear_caller_target_directory(&store, &id)?;
    let request = store.transaction(|state, _| Ok(state.find(&id)?.clone()))?;
    let target = target_directory(&policy, &request, true)?
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "target directory missing"))?;
    let canonical_root = target_root.canonicalize()?;
    assert_eq!(target.parent(), Some(canonical_root.as_path()));
    let mut command = Command::new("/usr/bin/env");
    command.env_remove("CARGO_TARGET_DIR");
    apply_target_environment(&mut command, &policy, &request)?;
    let output = command.output()?;
    assert!(output.status.success());
    let expected_target = format!("CARGO_TARGET_DIR={}", target.display());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == expected_target)
    );
    std::fs::write(target.join("artifact"), b"four")?;
    finish_started_request(&store, &id)?;

    cleanup_target(&store, &id)?;
    assert!(!target.exists());
    Ok(())
}

#[test]
fn caller_target_directory_survives_lease_cleanup() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&store.config_path)?)?;
    policy.target_root = Some(root.path().join("policy-targets"));
    policy.max_bytes = Some(1);
    std::fs::write(&store.config_path, serde_json::to_vec(&policy)?)?;

    let caller_target = root.path().join("caller-target");
    std::fs::create_dir(&caller_target)?;
    let caller_artifact = caller_target.join("artifact");
    std::fs::write(&caller_artifact, b"caller data")?;
    let id = queued(&store)?;
    store.transaction(|state, _| {
        state.find_mut(&id)?.caller_cargo_target_dir =
            Some(caller_target.to_string_lossy().into_owned());
        Ok(())
    })?;
    finish_started_request(&store, &id)?;

    cleanup_target(&store, &id)?;

    assert_eq!(std::fs::read(caller_artifact)?, b"caller data");
    assert!(!root.path().join("policy-targets").exists());
    Ok(())
}

#[test]
fn target_lifecycle_skips_symlinks_and_size_cleans_oversized_targets() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&store.config_path)?)?;
    policy.target_root = Some(root.path().join("targets"));
    policy.max_bytes = Some(1);
    std::fs::write(&store.config_path, serde_json::to_vec(&policy)?)?;

    let id = queued(&store)?;
    clear_caller_target_directory(&store, &id)?;
    let request = store.transaction(|state, _| Ok(state.find(&id)?.clone()))?;
    let target = target_directory(&policy, &request, true)?
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "target directory missing"))?;
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("keep"), b"safe")?;
    std::os::unix::fs::symlink(&outside, target.join("escape"))?;
    std::fs::write(target.join("small"), b"data")?;
    finish_started_request(&store, &id)?;

    cleanup_target(&store, &id)?;
    assert_eq!(std::fs::read(outside.join("keep"))?, b"safe");
    assert!(!target.exists());
    Ok(())
}

fn finish_started_request(store: &Store, id: &RequestId) -> Result<()> {
    store.transaction(|state, config| {
        let now = now_ms()?;
        let clock = clock_ms()?;
        let request = state.find_mut(id)?;
        request.phase = Phase::Running;
        request.started_at_ms = Some(now);
        request.started_clock_ms = Some(clock);
        state.finish(
            id,
            Completion {
                code: Some(0),
                ..Completion::default()
            },
            config,
            now,
            clock,
        )
    })
}

fn clear_caller_target_directory(store: &Store, id: &RequestId) -> Result<()> {
    store.transaction(|state, _| {
        state.find_mut(id)?.caller_cargo_target_dir = None;
        Ok(())
    })
}

#[test]
fn target_cleanup_treats_walk_bound_as_oversized_and_removes_symlink_tree() -> Result<()> {
    const TOO_MANY_ENTRIES: usize = 100_005;
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&store.config_path)?)?;
    policy.target_root = Some(root.path().join("targets"));
    policy.max_bytes = Some(1);
    std::fs::write(&store.config_path, serde_json::to_vec(&policy)?)?;

    let id = queued(&store)?;
    clear_caller_target_directory(&store, &id)?;
    let request = store.transaction(|state, _| Ok(state.find(&id)?.clone()))?;
    let target = target_directory(&policy, &request, true)?
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "target directory missing"))?;
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("keep"), b"safe")?;
    std::os::unix::fs::symlink(&outside, target.join("escape"))?;
    for index in 0..TOO_MANY_ENTRIES {
        std::fs::write(target.join(format!("artifact-{index}")), [])?;
    }
    finish_started_request(&store, &id)?;

    cleanup_target(&store, &id)?;
    assert!(!target.exists());
    assert_eq!(std::fs::read(outside.join("keep"))?, b"safe");
    Ok(())
}

/// Trace: FR-015-AC-10
#[test]
fn legacy_embedded_metrics_field_is_ignored_without_breaking_state_decode() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let mut encoded = store.transaction(|state, _| Ok(serde_json::to_value(state)?))?;
    let object = encoded
        .as_object_mut()
        .ok_or_else(|| Error::new(ErrorCode::State, "state JSON is not an object"))?;
    object.insert(
        "metrics".to_owned(),
        serde_json::json!({
            "builds": [
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
            ],
            "queue_wait": [
                {"buckets": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], "count": 0, "sum_ms": 0},
                {"buckets": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], "count": 0, "sum_ms": 0},
                {"buckets": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], "count": 0, "sum_ms": 0}
            ],
            "blocked_millis": [0, 0, 0, 0, 0],
            "active_block": null
        }),
    );
    let decoded: crate::model::State = serde_json::from_value(encoded)?;
    assert_eq!(decoded.schema_version, 1);
    Ok(())
}

/// Trace: FR-015-AC-10
#[test]
fn missing_metrics_sidecar_does_not_abort_build_state_transaction() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    std::fs::write(store.path.join("metrics.json"), "{broken")?;
    let id = queued(&store)?;
    assert_eq!(
        store.transaction(|state, _| Ok(state.find(&id)?.phase))?,
        Phase::Queued
    );
    Ok(())
}

/// Trace: FR-015-AC-10
#[test]
fn metrics_write_failure_after_state_commit_does_not_fail_enqueue() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    std::fs::create_dir(store.path.join("metrics.json.next"))?;
    let id = queued(&store)?;
    assert_eq!(
        store.transaction(|state, _| Ok(state.find(&id)?.phase))?,
        Phase::Queued
    );
    Ok(())
}

/// Trace: FR-015-AC-10
#[test]
fn metrics_sidecar_is_rebound_when_state_file_is_recreated() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let _id = queued(&store)?;
    let old: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.path.join("state.json"))?)?;
    let old_id = old["store_id"]
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::State, "store id missing"))?;
    std::fs::remove_file(store.path.join("state.json"))?;
    let _ = metrics(&store)?;
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.path.join("state.json"))?)?;
    let metrics_sidecar: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.path.join("metrics.json"))?)?;
    let new_id = state["store_id"]
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::State, "store id missing"))?;
    assert_ne!(old_id, new_id);
    assert_eq!(metrics_sidecar["store_id"].as_str(), Some(new_id));
    assert!(state["requests"].as_array().is_none_or(Vec::is_empty));
    Ok(())
}

/// Trace: FR-015-AC-11
#[test]
fn status_and_repeat_wait_polls_do_not_rewrite_unchanged_metrics_sidecar() -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let config_path = root.path().join("policy.json");
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    policy.memory_reserve_bytes = 2_000_000;
    std::fs::write(&config_path, serde_json::to_vec(&policy)?)?;
    let id = queued(&store)?;
    let identity = ProcessIdentity {
        pid: u32::MAX - 1,
        started_seconds: 1,
    };
    let fence = claim_worker(&store, &id, &identity)?
        .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
    let clock = TestClock::new()?;
    assert!(matches!(
        admit_at(&store, &id, &fence, clock)?,
        Admission::Waiting(_)
    ));
    let sidecar = store.path.join("metrics.json");
    let first_inode = std::fs::metadata(&sidecar)?.ino();
    let _ = status_at(&store, clock.now_ms, clock.clock_ms)?;
    assert_eq!(std::fs::metadata(&sidecar)?.ino(), first_inode);
    assert!(matches!(
        admit_at(&store, &id, &fence, clock)?,
        Admission::Waiting(_)
    ));
    assert_eq!(std::fs::metadata(&sidecar)?.ino(), first_inode);
    Ok(())
}

/// Trace: FR-015-AC-8
#[test]
fn priorities_above_two_are_admitted_and_counted_under_gate() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let config_path = root.path().join("policy.json");
    let policy: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    std::fs::write(&config_path, serde_json::to_vec(&policy)?)?;
    let id = enqueue(
        &store,
        "test:team:lead",
        "TEST-PRIORITY",
        1,
        3,
        Some("must-fix"),
        &["/usr/bin/true".into()],
    )?;
    let fence = claim_worker(
        &store,
        &id,
        &ProcessIdentity {
            pid: u32::MAX - 1,
            started_seconds: 1,
        },
    )?
    .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
    assert!(matches!(
        admit_worker(&store, &id, &fence, &AtomicBool::new(false))?,
        Admission::Granted(_)
    ));
    store.transaction(|state, config| {
        state.finish(
            &id,
            Completion {
                code: Some(0),
                ..Completion::default()
            },
            config,
            now_ms()?,
            clock_ms()?,
        )
    })?;
    assert!(metrics(&store)?.contains(
        "ix_build_builds_total{host=\"your-build-host\",class=\"must-fix\",outcome=\"ok\"} 1"
    ));
    Ok(())
}

/// Trace: FR-015-AC-3
#[test]
fn concurrent_textfile_exports_are_serialized_and_leave_a_complete_document() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let output = root.path().join("collector.prom");
    std::thread::scope(|scope| -> Result<()> {
        let mut exporters = Vec::new();
        for _ in 0..8 {
            exporters.push(scope.spawn(|| metrics_to_file(&store, &output)));
        }
        for exporter in exporters {
            exporter
                .join()
                .map_err(|_| Error::new(ErrorCode::State, "metrics exporter panicked"))??;
        }
        Ok(())
    })?;
    assert_eq!(std::fs::read_to_string(&output)?, metrics(&store)?);
    let leftovers = std::fs::read_dir(root.path())?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(leftovers, 0);
    Ok(())
}

/// Trace: FR-015-AC-5
#[test]
fn metrics_scrape_completes_while_a_request_is_being_admitted() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    let fence = claim_worker(
        &store,
        &id,
        &ProcessIdentity {
            pid: u32::MAX - 1,
            started_seconds: 1,
        },
    )?
    .ok_or_else(|| Error::new(ErrorCode::State, "worker claim missing"))?;
    let clock = TestClock::new()?;
    let barrier = std::sync::Barrier::new(2);
    let (scrape_sender, scrape_receiver) = std::sync::mpsc::channel();
    let (admit_sender, admit_receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| -> Result<()> {
        let scraper = scope.spawn(|| {
            barrier.wait();
            let result = metrics_at_clock(&store, clock);
            let _ = scrape_sender.send(result);
        });
        let admitting = scope.spawn(|| {
            barrier.wait();
            let result = admit_at(&store, &id, &fence, clock);
            let _ = admit_sender.send(result);
        });
        let scrape = scrape_receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|error| Error::new(ErrorCode::State, error.to_string()))??;
        let admitted = admit_receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|error| Error::new(ErrorCode::State, error.to_string()))??;
        scraper
            .join()
            .map_err(|_| Error::new(ErrorCode::State, "metrics scrape thread panicked"))?;
        admitting
            .join()
            .map_err(|_| Error::new(ErrorCode::State, "admission thread panicked"))?;
        assert!(scrape.contains("# TYPE ix_build_queue_depth gauge"));
        assert!(scrape.contains("ix_build_slots_busy{host=\"your-build-host\"}"));
        assert!(matches!(admitted, Admission::Granted(_)));
        Ok(())
    })?;
    Ok(())
}

/// Trace: FR-015-AC-1
#[test]
fn holding_textfile_export_lock_does_not_hold_the_scheduler_state_lock() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let output = root.path().join("collector.prom");
    let export_lock = store.lock_metrics_export()?;
    let id = enqueue(
        &store,
        "test:team:lead",
        "TEST-DURING-EXPORT",
        1,
        0,
        None,
        &["/usr/bin/true".into()],
    )?;
    assert_eq!(state_phase(&store, &id)?, Phase::Queued);
    drop(export_lock);
    metrics_to_file(&store, &output)?;
    assert!(output.exists());
    Ok(())
}

/// Trace: FR-005-AC-2, FR-005-AC-4
#[test]
fn recovery_snapshot_and_state_lock_do_not_erase_an_interleaved_claim() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    let identity = ProcessIdentity {
        pid: u32::MAX - 1,
        started_seconds: 1,
    };
    let (sampled_tx, sampled_rx) = mpsc::sync_channel(0);
    let (resume_tx, resume_rx) = mpsc::sync_channel(0);
    std::thread::scope(|scope| -> Result<()> {
        let recovery_store = &store;
        let handle = scope.spawn(move || {
            recover_with_processes(recovery_store, || {
                let processes = Processes::read();
                assert!(sampled_tx.send(()).is_ok());
                assert!(resume_rx.recv().is_ok());
                processes
            })
        });
        sampled_rx
            .recv()
            .map_err(|_| Error::new(ErrorCode::State, "sample checkpoint lost"))?;
        let lock = File::options()
            .read(true)
            .write(true)
            .open(store.path.join("lock"))?;
        let claimed_before_resume = match lock.try_lock() {
            Ok(()) => {
                lock.unlock()?;
                assert!(claim_worker(&store, &id, &identity)?.is_some());
                true
            }
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Error(error)) => return Err(error.into()),
        };
        resume_tx
            .send(())
            .map_err(|_| Error::new(ErrorCode::State, "recovery resume lost"))?;
        handle
            .join()
            .map_err(|_| Error::new(ErrorCode::State, "recovery thread panicked"))??;
        if !claimed_before_resume {
            assert!(claim_worker(&store, &id, &identity)?.is_some());
        }
        store.transaction(|state, _| {
            assert_eq!(
                state.find(&id)?.worker.as_ref(),
                Some(&identity),
                "a snapshot collected before the state lock erased a newer worker claim"
            );
            Ok(())
        })
    })
}

/// Trace: FR-005-AC-2, FR-005-AC-4
#[test]
fn superseded_worker_cannot_admit_or_complete_a_replacement_generation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    let identity = ProcessIdentity {
        pid: u32::MAX - 1,
        started_seconds: 1,
    };
    let old = claim_worker(&store, &id, &identity)?
        .ok_or_else(|| Error::new(ErrorCode::State, "claim missing"))?;
    recover(&store)?;
    // Reuse even the same synthetic identity: the generation must still fence it.
    let replacement = claim_worker(&store, &id, &identity)?
        .ok_or_else(|| Error::new(ErrorCode::State, "replacement missing"))?;
    assert!(replacement.generation > old.generation);
    let interrupted = AtomicBool::new(false);
    assert!(matches!(
        admit_worker(&store, &id, &old, &interrupted)?,
        Admission::Stopped
    ));
    store.transaction(|state, _| {
        assert_eq!(state.used(), 0);
        Ok(())
    })?;
    assert!(matches!(
        admit_worker(&store, &id, &replacement, &interrupted)?,
        Admission::Granted(_)
    ));
    assert!(
        store
            .transaction(|state, config| finish_owned(
                state,
                config,
                &id,
                &old,
                u32::MAX - 1,
                Completion {
                    code: Some(0),
                    ..Completion::default()
                }
            ))
            .is_err()
    );
    store.transaction(|state, _| {
        assert_eq!(
            state.used(),
            1,
            "a stale completion released the replacement lease"
        );
        assert_eq!(state.find(&id)?.worker_generation, replacement.generation);
        Ok(())
    })
}

/// Trace: FR-005-AC-2, FR-005-AC-4
#[test]
fn recovery_keeps_actual_occupancy_for_a_command_born_after_pid_capture() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    let snapshot = Processes::read();
    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()?;
    let identity = ProcessIdentity {
        pid: child.id(),
        started_seconds: 1,
    };
    let fence = claim_worker(&store, &id, &identity)?
        .ok_or_else(|| Error::new(ErrorCode::State, "claim missing"))?;
    let admission = admit_worker(&store, &id, &fence, &AtomicBool::new(false))?;
    assert!(matches!(admission, Admission::Granted(_)));
    recover_with_processes(&store, || snapshot)?;
    let used_while_alive = store.transaction(|state, _| Ok(state.used()))?;
    let alive = child.try_wait()?.is_none();
    child.kill()?;
    child.wait()?;
    assert!(
        alive,
        "the missed process must still occupy its group at the decision"
    );
    assert_eq!(
        used_while_alive, 1,
        "a non-atomic enumeration released occupied capacity"
    );
    recover(&store)?;
    store.transaction(|state, _| {
        assert_eq!(
            state.used(),
            0,
            "kernel-confirmed group exit must reclaim the lease"
        );
        Ok(())
    })
}

/// Trace: FR-015-AC-3, FR-015-AC-10
#[test]
fn outcome_counters_survive_store_reopen_and_prune() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    let recovered_id = queued(&store)?;
    store.transaction(|state, config| {
        state.find_mut(&id)?.stop = Some(crate::model::StopReason::Cancelled);
        state.finish(&id, Completion::default(), config, now_ms()?, clock_ms()?)?;
        let request = state.find_mut(&recovered_id)?;
        request.phase = Phase::Running;
        request.started_at_ms = Some(now_ms()?);
        request.started_clock_ms = Some(clock_ms()?);
        request.worker = Some(ProcessIdentity {
            pid: u32::MAX - 1,
            started_seconds: 1,
        });
        request.worker_generation = 1;
        state.boot_time = u64::MAX;
        Ok(())
    })?;
    recover(&store)?;
    let after_first_recovery = metric_outcome_lines(&metrics(&store)?);
    drop(store);

    let reopened = Store::open(&root.path().join("queue"), &root.path().join("policy.json"))?;
    recover(&reopened)?;
    let before_prune = metric_outcome_lines(&metrics(&reopened)?);
    assert_eq!(after_first_recovery, before_prune);
    assert_eq!(prune(&reopened)?, 2);
    let output = metrics(&reopened)?;
    assert_eq!(before_prune, metric_outcome_lines(&output));
    assert!(output.contains(
        "ix_build_builds_total{host=\"your-build-host\",class=\"periodic\",outcome=\"cancelled\"} 1"
    ));
    assert!(output.contains(
        "ix_build_builds_total{host=\"your-build-host\",class=\"periodic\",outcome=\"aborted_thermal\"} 0"
    ));
    assert!(output.contains(
        "ix_build_builds_total{host=\"your-build-host\",class=\"periodic\",outcome=\"failed\"} 1"
    ));
    Ok(())
}

fn metric_outcome_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("ix_build_builds_total{"))
        .map(str::to_owned)
        .collect()
}
