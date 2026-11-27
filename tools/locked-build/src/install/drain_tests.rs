// SPDX-License-Identifier: MIT
//! Installation drain interleavings against a real locked store and artifact.
use super::*;
use crate::model::{Completion, Phase, RequestId};
use crate::store::Store;

struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
    artifact: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let policy = root.path().join("policy.json");
        std::fs::write(
            &policy,
            serde_json::to_vec(&Config {
                max_queued_per_owner: Some(10),
                ..Config::default()
            })?,
        )?;
        let store = Store::open(&root.path().join("shared"), &policy)?;
        let artifact = root.path().join("artifact");
        std::fs::write(&artifact, b"new artifact")?;
        let home = root.path().join("home");
        std::fs::create_dir_all(home.join(".local/bin"))?;
        std::fs::write(home.join(".local/bin/locked-build"), b"old artifact")?;
        Ok(Self {
            _root: root,
            store,
            artifact,
            home,
        })
    }

    fn enqueue(&self) -> Result<RequestId> {
        crate::runtime::enqueue(
            &self.store,
            "drain-fixture",
            "SWM-429",
            1,
            0,
            None,
            &["true".into()],
        )
    }

    fn install(
        &self,
        timeout: Duration,
        prepare: impl FnOnce() -> Result<()>,
        finish: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        install_tools_owned(
            &self.home,
            &self.artifact,
            None,
            &self.store.path,
            "fixture-rollout",
            None,
            "1111111111111111111111111111111111111111",
            Some(timeout),
            || Ok(Config::default()),
            prepare,
            finish,
            |election, _| {
                assert!(self.store.try_admission_lock()?.is_none());
                assert!(self.store.try_lock_member("lock")?.is_none());
                assert!(self.store.try_lock_member("scheduler.lock")?.is_none());
                drop(election);
                Ok(None)
            },
        )
    }
}

/// Provenance: SWM-429. Trace: FR-011-AC-7, FR-005-AC-4
#[test]
fn install_drain_waits_for_completion_then_swaps_without_touching_queued_requests() -> Result<()> {
    let fixture = Fixture::new()?;
    let running = fixture.enqueue()?;
    let queued = fixture.enqueue()?;
    fixture.store.transaction(|state, _| {
        let request = state.find_mut(&running)?;
        request.phase = Phase::Running;
        request.worker = Some(crate::process::Processes::self_identity()?);
        request.worker_generation = 1;
        request.started_at_ms = Some(crate::runtime::now_ms()?);
        request.started_clock_ms = Some(crate::runtime::clock_ms()?);
        Ok(())
    })?;
    let queued_before = serde_json::to_value(fixture.store.snapshot()?.0.find(&queued)?)?;
    std::thread::scope(|scope| -> Result<()> {
        let installer = scope.spawn(|| {
            fixture.install(
                Duration::from_secs(3),
                || {
                    assert!(fixture.store.try_admission_lock()?.is_none());
                    assert_eq!(
                        std::fs::read(fixture.home.join(".local/bin/locked-build"))?,
                        b"old artifact"
                    );
                    Ok(())
                },
                || {
                    assert!(fixture.store.try_admission_lock()?.is_none());
                    assert_eq!(
                        std::fs::read(fixture.home.join(".local/bin/locked-build"))?,
                        b"new artifact"
                    );
                    Ok(())
                },
            )
        });
        let started = Instant::now();
        while fixture.store.try_admission_lock()?.is_some() {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "drain did not acquire hold"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            std::fs::read(fixture.home.join(".local/bin/locked-build"))?,
            b"old artifact"
        );
        let newcomer = fixture.enqueue()?;
        fixture.store.transaction(|state, config| {
            assert_eq!(state.find(&running)?.phase, Phase::Running);
            assert!(state.requests.iter().all(|request| request.stop.is_none()));
            state.finish(
                &running,
                Completion {
                    code: Some(0),
                    ..Completion::default()
                },
                config,
                crate::runtime::now_ms()?,
                crate::runtime::clock_ms()?,
            )
        })?;
        installer
            .join()
            .map_err(|_| Error::new(ErrorCode::State, "installer fixture panicked"))??;
        let (state, _) = fixture.store.snapshot()?;
        assert_eq!(serde_json::to_value(state.find(&queued)?)?, queued_before);
        assert_eq!(state.find(&newcomer)?.phase, Phase::Queued);
        assert!(state.find(&queued)?.sequence < state.find(&newcomer)?.sequence);
        assert_eq!(state.find(&running)?.phase, Phase::Succeeded);
        Ok(())
    })?;
    assert!(fixture.store.try_admission_lock()?.is_some());
    assert!(crate::provenance::check(&fixture.home).is_ok());
    Ok(())
}

/// Provenance: SWM-429. Trace: FR-011-AC-7, FR-005-AC-4
#[test]
fn install_drain_timeout_releases_hold_and_keeps_running_and_queued_records() -> Result<()> {
    let fixture = Fixture::new()?;
    let running = fixture.enqueue()?;
    fixture.enqueue()?;
    fixture.store.transaction(|state, _| {
        let request = state.find_mut(&running)?;
        request.phase = Phase::Running;
        request.worker = Some(crate::process::Processes::self_identity()?);
        request.worker_generation = 1;
        request.started_at_ms = Some(crate::runtime::now_ms()?);
        request.started_clock_ms = Some(crate::runtime::clock_ms()?);
        Ok(())
    })?;
    let before = std::fs::read(fixture.store.path.join("state.json"))?;
    let started = Instant::now();
    let result = fixture.install(
        Duration::from_millis(40),
        || panic!("must not prepare before drain"),
        || Ok(()),
    );
    assert!(result.is_err_and(|error| error.message.contains("drain timed out")));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        std::fs::read(fixture.store.path.join("state.json"))?,
        before
    );
    assert_eq!(
        std::fs::read(fixture.home.join(".local/bin/locked-build"))?,
        b"old artifact"
    );
    assert!(fixture.store.try_admission_lock()?.is_some());
    Ok(())
}

/// Provenance: SWM-429. Trace: FR-011-AC-7, FR-005-AC-4
#[test]
fn install_drain_releases_hold_on_failures_before_and_after_binary_swap() -> Result<()> {
    for fail_after_swap in [false, true] {
        let fixture = Fixture::new()?;
        fixture.enqueue()?;
        let failure = || Err(Error::new(ErrorCode::Io, "fixture installation failed"));
        let result = fixture.install(
            Duration::from_secs(1),
            || {
                assert!(fixture.store.try_admission_lock()?.is_none());
                if fail_after_swap { Ok(()) } else { failure() }
            },
            || {
                assert!(fixture.store.try_admission_lock()?.is_none());
                failure()
            },
        );
        assert!(result.is_err_and(|error| error.message.as_ref() == "fixture installation failed"));
        assert!(fixture.store.try_admission_lock()?.is_some());
        assert!(fixture.store.try_lock_member("lock")?.is_some());
        assert_eq!(
            std::fs::read(fixture.home.join(".local/bin/locked-build"))?,
            b"old artifact"
        );
        assert_eq!(fixture.store.snapshot()?.0.requests[0].phase, Phase::Queued);
        assert!(!fixture.home.join(".ix/locked-build/install.json").exists());
    }
    Ok(())
}

/// Provenance: SWM-429. Trace: FR-011-AC-7, FR-005-AC-4
#[test]
fn install_drain_bounds_state_lock_wait_and_fails_closed_on_corrupt_state() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.enqueue()?;
    let writer = fixture.store.try_lock_member("lock")?;
    let result = fixture.install(
        Duration::from_millis(40),
        || panic!("state lock unavailable"),
        || Ok(()),
    );
    assert!(result.is_err_and(|error| error.message.contains("drain timed out")));
    assert!(fixture.store.try_admission_lock()?.is_some());
    drop(writer);
    std::fs::write(fixture.store.path.join("state.json"), b"invalid state")?;
    assert!(
        fixture
            .install(
                Duration::from_secs(1),
                || panic!("corrupt state"),
                || Ok(())
            )
            .is_err()
    );
    assert!(fixture.store.try_admission_lock()?.is_some());
    assert_eq!(
        std::fs::read(fixture.home.join(".local/bin/locked-build"))?,
        b"old artifact"
    );
    Ok(())
}

/// Provenance: SWM-429. Trace: FR-011-AC-7, FR-005-AC-4
#[test]
fn install_drain_refuses_unsupported_scheduler_and_queued_worker_without_swapping() -> Result<()> {
    for legacy_scheduler in [true, false] {
        let fixture = Fixture::new()?;
        let id = fixture.enqueue()?;
        let guard = if legacy_scheduler {
            fixture.store.try_lock_member("scheduler.lock")?
        } else {
            let identity = crate::process::Processes::self_identity()?;
            fixture.store.transaction(|state, _| {
                let request = state.find_mut(&id)?;
                request.worker = Some(identity.clone());
                request.worker_generation = 1;
                fixture.store.write_claim(&crate::model::WorkerClaim {
                    protocol: 1,
                    store_id: state.store_id.clone(),
                    request_id: id.clone(),
                    generation: 1,
                    launcher: identity.clone(),
                    pid_namespace: crate::process::pid_namespace()?,
                    worker: Some(identity.clone()),
                })
            })?;
            fixture
                .store
                .try_lock_member(&format!("{}.spawn.lock", id.0))?
        };
        let before = std::fs::read(fixture.store.path.join("state.json"))?;
        let result = fixture.install(
            Duration::from_secs(1),
            || panic!("unsupported participant"),
            || Ok(()),
        );
        assert!(
            result.is_err_and(|error| error.message.contains("does not support install drain"))
        );
        assert_eq!(
            std::fs::read(fixture.store.path.join("state.json"))?,
            before
        );
        assert_eq!(
            std::fs::read(fixture.home.join(".local/bin/locked-build"))?,
            b"old artifact"
        );
        assert!(fixture.store.try_admission_lock()?.is_some());
        drop(guard);
    }
    Ok(())
}

/// Provenance: SWM-429. Trace: FR-011-AC-7, FR-005-AC-4
#[test]
fn install_drain_keeps_durable_hold_after_installer_is_killed_until_explicit_resume() -> Result<()>
{
    let fixture = Fixture::new()?;
    let ready = fixture._root.path().join("ready");
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "install::drain_tests::install_drain_hold_subprocess",
            "--ignored",
            "--test-threads=4",
        ])
        .env("IX_DRAIN_FIXTURE_STATE", &fixture.store.path)
        .env("IX_DRAIN_FIXTURE_READY", &ready)
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let started = Instant::now();
    while !ready.exists() && started.elapsed() < Duration::from_secs(3) {
        if child.try_wait()?.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let was_ready = ready.exists();
    let held = fixture.store.try_admission_lock()?.is_none();
    let _ = child.kill();
    child.wait()?;
    assert!(
        was_ready && held,
        "installer fixture failed to acquire hold"
    );
    assert!(fixture.store.try_admission_lock()?.is_none());
    assert!(fixture.store.try_lock_member("scheduler.lock")?.is_some());
    assert!(fixture.store.try_lock_member("lock")?.is_some());
    crate::queue::resume(&fixture.store)?;
    assert!(fixture.store.try_admission_lock()?.is_some());
    Ok(())
}

/// Provenance: SWM-429. Trace: FR-011-AC-7, FR-005-AC-4
#[test]
#[ignore = "owned installer child for abrupt-exit lock cleanup test"]
fn install_drain_hold_subprocess() -> Result<()> {
    let shared = std::env::var_os("IX_DRAIN_FIXTURE_STATE")
        .ok_or_else(|| Error::new(ErrorCode::State, "fixture state missing"))?;
    let _hold = install_admission_hold(Path::new(&shared), Some(Duration::from_secs(1)))?;
    let ready = std::env::var_os("IX_DRAIN_FIXTURE_READY")
        .ok_or_else(|| Error::new(ErrorCode::State, "fixture ready missing"))?;
    std::fs::write(ready, b"held")?;
    loop {
        std::thread::park();
    }
}
