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
        state.boot_time = 0;
        Ok(())
    })?;
    recover(&store)?;
    drop(store);

    let reopened = Store::open(&root.path().join("queue"), &root.path().join("policy.json"))?;
    recover(&reopened)?;
    assert_eq!(prune(&reopened)?, 2);
    let output = metrics(&reopened)?;
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
