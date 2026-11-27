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

pub(crate) fn ensure(store: &Store) -> Result<()> {
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
    let stderr = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(store.path.join("scheduler.stderr"))?;
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--state")
        .arg(&store.path)
        .arg("--config")
        .arg(&store.config_path)
        .arg("scheduler")
        .stdin(Stdio::from(lease))
        .stdout(Stdio::null())
        .stderr(stderr)
        .process_group(0)
        .spawn()?;
    // Observe this detached service child; callers need not wait for the whole
    // queue after their own request has finished.
    std::thread::spawn(move || {
        if let Err(error) = child.wait() {
            eprintln!("locked-build: scheduler child wait failed: {error}");
        }
    });
    Ok(())
}

pub(crate) fn run(store: &Store) -> Result<()> {
    let Some(mut lease) = store.inherited_or_try_lock(ELECTION_LOCK)? else {
        return Ok(());
    };
    // Diagnostic owner follows the restart timestamp. The OS lock, never this
    // PID, determines election ownership.
    lease.seek(SeekFrom::Start(8))?;
    lease.write_all(&std::process::id().to_le_bytes())?;
    lease.set_len(12)?;
    let mut children: Vec<Child> = Vec::new();
    let mut recovered = None;
    loop {
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
