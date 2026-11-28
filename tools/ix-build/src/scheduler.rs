// SPDX-License-Identifier: MIT
//! One elected, short-lived coordinator per store. Queued workers only await hints.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::model::Phase;
use crate::runtime;
use crate::store::Store;

const ELECTION_LOCK: &str = "scheduler.lock";
pub(crate) const DRAIN_PROTOCOL: &[u8] = b"install-drain-v1";

pub(crate) fn ensure(store: &Store) -> Result<()> {
    let Some(_admission) = store.try_admission_lock()? else {
        return Ok(());
    };
    let Some(mut lease) = store.try_lock_member(ELECTION_LOCK)? else {
        return Ok(());
    };
    // Bound restart storms too, not just concurrent live coordinators.
    let now = runtime::clock_ms()?;
    let mut previous = [0_u8; 8];
    if lease.read_exact(&mut previous).is_ok() {
        let previous = u64::from_le_bytes(previous);
        if now >= previous && now - previous < crate::config::POLL_INTERVAL_MS {
            return Ok(());
        }
    }
    lease.seek(SeekFrom::Start(0))?;
    lease.write_all(&now.to_le_bytes())?;
    lease.set_len(8)?;
    let child = spawn(store, &store.service_binary()?, lease, false, None)?;
    detach(child);
    Ok(())
}

/// Transfers the owned election to the installed successor without an election
/// gap in which an old queued caller could start its replaced executable.
/// Provenance: SWM-429. Trace: FR-011-AC-7, FR-005-AC-4
pub(crate) fn handoff(
    store: &Store,
    binary: &std::path::Path,
    lease: std::fs::File,
    token: &str,
) -> Result<Child> {
    spawn(store, binary, lease, true, Some(token))
}

fn spawn(
    store: &Store,
    binary: &std::path::Path,
    lease: std::fs::File,
    wait: bool,
    token: Option<&str>,
) -> Result<Child> {
    let stderr = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(store.path.join("scheduler.stderr"))?;
    let mut command = Command::new(binary);
    command
        .arg("--state")
        .arg(&store.path)
        .arg("--config")
        .arg(&store.config_path)
        .arg("scheduler");
    if wait {
        command.arg("--wait-for-admission");
    }
    if let Some(token) = token {
        command.arg("--install-token").arg(token);
    }
    Ok(command
        .stdin(Stdio::from(lease))
        .stdout(Stdio::null())
        .stderr(stderr)
        .process_group(0)
        .spawn()?)
}

pub(crate) fn detach(mut child: Child) {
    // Observe this detached service child; callers need not wait for the whole
    // queue after their own request has finished.
    std::thread::spawn(move || {
        if let Err(error) = child.wait() {
            eprintln!("ix-build: scheduler child wait failed: {error}");
        }
    });
}

pub(crate) fn run(store: &Store, wait_for_admission: bool, token: Option<&str>) -> Result<()> {
    let Some(mut lease) = store.inherited_or_try_lock(ELECTION_LOCK)? else {
        return Ok(());
    };
    // Diagnostic owner follows the restart timestamp. The OS lock, never this
    // PID, determines election ownership.
    lease.seek(SeekFrom::Start(8))?;
    lease.write_all(&std::process::id().to_le_bytes())?;
    lease.write_all(DRAIN_PROTOCOL)?;
    lease.set_len(12 + DRAIN_PROTOCOL.len() as u64)?;
    if wait_for_admission {
        while store.try_admission_lock()?.is_none() {
            std::thread::sleep(Duration::from_millis(crate::config::POLL_INTERVAL_MS));
        }
    }
    if let Some(token) = token {
        let committed: Option<String> = std::fs::read(store.path.join("install-committed.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        if committed.as_deref() != Some(token) {
            return Ok(());
        }
    }
    let mut children: Vec<Child> = Vec::new();
    let mut recovered = None;
    loop {
        // The installer will inherit election ownership and restart us after
        // the swap. Queued workers yield too; running leases keep supervising.
        if store.try_admission_lock()?.is_none() {
            return Ok(());
        }
        let mut active = Vec::new();
        for mut child in children {
            if child.try_wait()?.is_none() {
                active.push(child);
            }
        }
        children = active;
        if recovered.is_none_or(|last: Instant| last.elapsed() >= Duration::from_secs(2)) {
            for id in runtime::recover(store)? {
                if let Some(child) = runtime::spawn_worker(store, &id)? {
                    children.push(child);
                }
            }
            recovered = Some(Instant::now());
        }
        runtime::schedule_queue(store)?;
        let (state, _) = store.snapshot()?;
        store.repair_notices(&state);
        if !state
            .requests
            .iter()
            .any(|request| request.phase == Phase::Queued)
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(crate::config::POLL_INTERVAL_MS));
    }
}
