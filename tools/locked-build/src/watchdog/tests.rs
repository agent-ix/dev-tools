// SPDX-License-Identifier: MIT
//! Controlled command birth and descendant fork after a process snapshot.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Child;

use super::*;

#[test]
fn only_rss_watchdog_enforcement_uses_the_process_rss_cap() {
    assert_eq!(
        rss_fallback_cap(MemoryEnforcement::RssWatchdog, Some(100), Some(200)),
        Some(100)
    );
    assert_eq!(
        rss_fallback_cap(MemoryEnforcement::RssWatchdog, None, Some(200)),
        Some(200)
    );
    assert_eq!(
        rss_fallback_cap(MemoryEnforcement::SystemdScope, Some(100), Some(200)),
        None
    );
}

fn fixture(mode: &str, root: &Path) -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "watchdog::tests::pipe_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("LOCKED_BUILD_PROBE_MODE", mode)
        .env("LOCKED_BUILD_PROBE_ROOT", root);
    Ok(command)
}

fn scenario(mode: &str) -> Result<()> {
    let root = tempfile::tempdir()?;
    let output = fixture(mode, root.path())?.process_group(0).output()?;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Trace: FR-005-AC-2, FR-005-AC-4
#[test]
fn missed_command_birth_cannot_release_occupied_watchdog_lease() -> Result<()> {
    scenario("direct")
}

/// Trace: FR-005-AC-2, FR-005-AC-4
#[test]
fn missed_descendant_fork_and_parent_exit_cannot_release_occupied_watchdog_lease() -> Result<()> {
    scenario("descendant")
}

fn watchdog_snapshot_scenario(root: &Path, mode: &str) -> Result<()> {
    let config = root.join("policy.json");
    fs::write(&config, include_bytes!("../../policy.example.json"))?;
    let store = Store::open(&root.join("state"), &config)?;
    let id = crate::runtime::enqueue(
        &store,
        "test:team:lead",
        "TEST-1",
        1,
        0,
        None,
        &["unused".into()],
    )?;
    let watchdog = Processes::read().current()?;
    let worker = ProcessIdentity {
        pid: u32::MAX - 1,
        started_seconds: 1,
    };
    store.transaction(|state, _| {
        let request = state.find_mut(&id)?;
        request.worker = Some(worker.clone());
        request.worker_generation = 1;
        request.watchdog = Some(watchdog.clone());
        request.phase = Phase::Running;
        request.started_at_ms = Some(now_ms()?);
        request.started_clock_ms = Some(clock_ms()?);
        Ok(())
    })?;
    // Capture the PID population before creation; the finished snapshot omits
    // the command even though its supervisor is already absent.
    let snapshot = Processes::read();
    let child_mode = if mode == "direct" { "leaf" } else { "fork" };
    let mut child = fixture(child_mode, root)?
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_for_leaf(root, &mut child, mode == "descendant")?;
    let decision = observe(
        &store,
        &id,
        1,
        &worker,
        &watchdog,
        || snapshot,
        |group| probe_with_parking(group, &mut fixture("parking", root)?),
    )?;
    let occupied = store.transaction(|state, _| Ok(state.used()))?;
    let leaf_pid: i32 = fs::read_to_string(root.join("leaf"))?.parse().map_err(
        |error: std::num::ParseIntError| Error::new(ErrorCode::State, error.to_string()),
    )?;
    let leaf_alive = nix::sys::signal::kill(Pid::from_raw(leaf_pid), None).is_ok();
    drop(child.stdin.take());
    child.wait()?;
    assert!(
        leaf_alive,
        "the missed command must still be alive at the decision"
    );
    assert_eq!(
        occupied, 1,
        "watchdog enumeration released actual occupied capacity"
    );
    assert!(
        matches!(decision, Decision::Observe { .. }),
        "a missed live command completed the lease"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let decision = observe(
            &store,
            &id,
            1,
            &worker,
            &watchdog,
            Processes::read,
            |group| probe_with_parking(group, &mut fixture("parking", root)?),
        )?;
        if matches!(decision, Decision::Done) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "verified command exit did not reclaim capacity"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    store.transaction(|state, _| {
        assert_eq!(state.used(), 0);
        Ok(())
    })
}

fn wait_for_leaf(root: &Path, child: &mut Child, parent_exits: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("leaf").exists() || (parent_exits && child.try_wait()?.is_none()) {
        assert!(
            Instant::now() < deadline,
            "fixture did not start its leaf and exit its parent"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[test]
#[ignore = "isolated process-group fixture invoked by watchdog tests"]
fn pipe_fixture() -> Result<()> {
    let mode = std::env::var("LOCKED_BUILD_PROBE_MODE")
        .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let root = PathBuf::from(
        std::env::var_os("LOCKED_BUILD_PROBE_ROOT")
            .ok_or_else(|| Error::new(ErrorCode::State, "fixture root missing"))?,
    );
    match mode.as_str() {
        "parking" => {
            std::io::copy(&mut std::io::stdin(), &mut std::io::sink())?;
        }
        "leaf" => {
            fs::write(root.join("leaf"), std::process::id().to_string())?;
            std::io::copy(&mut std::io::stdin(), &mut std::io::sink())?;
        }
        "fork" => {
            fixture("leaf", &root)?.stdin(Stdio::inherit()).spawn()?;
        }
        "direct" | "descendant" => watchdog_snapshot_scenario(&root, &mode)?,
        _ => return Err(Error::new(ErrorCode::State, "unknown fixture mode")),
    }
    Ok(())
}
