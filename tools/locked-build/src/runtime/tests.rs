// SPDX-License-Identifier: MIT
//! Controlled recovery/claim interleavings and generation-fence regressions.

use std::fs::{File, TryLockError};
use std::sync::mpsc;

use super::*;
use crate::config::Config;
use crate::model::ProcessIdentity;

fn store(root: &Path) -> Result<Store> {
    let mut policy: Config = serde_json::from_str(include_str!("../../policy.example.json"))?;
    policy.capacity_units = 1;
    policy.memory_per_unit_bytes = 1;
    policy.memory_reserve_bytes = 0;
    policy.disk_reserve_bytes = 0;
    policy.maximum_load = 1_000_000.0;
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

fn queued(store: &Store) -> Result<RequestId> {
    enqueue(
        store,
        "test:team:lead",
        "TEST-1",
        1,
        0,
        &["/usr/bin/true".into()],
    )
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
        "ix_build_queue_wait_seconds_count{host=\"your-build-host\",priority=\"check\"} 1"
    ));
    assert!(
        admitted.contains("ix_build_queue_depth{host=\"your-build-host\",priority=\"check\"} 0")
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
        "ix_build_builds_total{host=\"your-build-host\",priority=\"check\",outcome=\"ok\"} 1"
    ));
    assert!(completed.contains("ix_build_slots_busy{host=\"your-build-host\"} 0"));
    let state_bytes = std::fs::read_to_string(store.path.join("state.json"))?;
    assert!(!state_bytes.contains("\"metrics\""));
    Ok(())
}

/// Trace: FR-015-AC-1
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
    assert!(matches!(
        admit_worker(&store, &first, &first_fence, &AtomicBool::new(false))?,
        Admission::Waiting(_)
    ));
    assert!(matches!(
        admit_worker(&store, &second, &second_fence, &AtomicBool::new(false))?,
        Admission::Waiting(_)
    ));

    let blocked = metrics(&store)?;
    assert!(blocked.contains("reason=\"memory\""));
    let blocked_value = metric_value(
        &blocked,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    assert!(blocked_value > 0.0);

    cancel(&store, &first)?;
    cancel(&store, &second)?;
    let closed = metrics(&store)?;
    let closed_value = metric_value(
        &closed,
        "ix_build_admission_blocked_seconds_total{host=\"your-build-host\",reason=\"memory\"} ",
    )
    .ok_or_else(|| Error::new(ErrorCode::State, "memory blocked series missing"))?
    .parse::<f64>()
    .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    std::thread::sleep(Duration::from_millis(20));
    let later = metrics(&store)?;
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

fn metric_value<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines().find_map(|line| line.strip_prefix(prefix))
}

#[test]
fn metrics_open_refuses_a_missing_state_path_without_creating_it() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = root.path().join("policy.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&Config {
            host: "host".to_owned(),
            capacity_units: 1,
            maximum_units: 1,
            memory_per_unit_bytes: 1,
            memory_reserve_bytes: 0,
            disk_reserve_bytes: 0,
            disk_path: root.path().to_owned(),
            maximum_load: 1.0,
            maximum_priority: 2,
            aging_ms: 1,
            hold_ms: 1,
            termination_grace_ms: 1,
            sample_max_age_ms: 1,
            heartbeat_max_age_ms: 2,
            poll_ms: 1,
            maximum_requests: 1,
            sample_file: None,
        })?,
    )?;
    let missing = root.path().join("not-created");
    assert!(Store::open_existing(&missing, &config).is_err());
    assert!(!missing.exists());
    Ok(())
}

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

#[test]
fn priorities_above_two_are_admitted_and_counted_under_other() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let config_path = root.path().join("policy.json");
    let mut policy: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
    policy.maximum_priority = 5;
    std::fs::write(&config_path, serde_json::to_vec(&policy)?)?;
    let id = enqueue(
        &store,
        "test:team:lead",
        "TEST-PRIORITY",
        1,
        3,
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
        "ix_build_builds_total{host=\"your-build-host\",priority=\"other\",outcome=\"ok\"} 1"
    ));
    Ok(())
}

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
        "ix_build_builds_total{host=\"your-build-host\",priority=\"check\",outcome=\"cancelled\"} 1"
    ));
    assert!(output.contains(
        "ix_build_builds_total{host=\"your-build-host\",priority=\"check\",outcome=\"aborted_thermal\"} 0"
    ));
    assert!(output.contains(
        "ix_build_builds_total{host=\"your-build-host\",priority=\"check\",outcome=\"failed\"} 1"
    ));
    Ok(())
}

fn metric_outcome_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("ix_build_builds_total{"))
        .map(str::to_owned)
        .collect()
}
