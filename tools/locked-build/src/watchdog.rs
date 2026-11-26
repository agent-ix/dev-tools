// SPDX-License-Identifier: MIT
//! A command-group leader keeps cancellation and deadlines alive after worker loss.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use nix::unistd::{Pid, setpgid};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Completion, MemoryEnforcement, Phase, ProcessIdentity, RequestId, StopReason};
use crate::process::{IdentityState, Processes, group_absent};
use crate::runtime::{clock_ms, now_ms};
use crate::store::Store;

pub(crate) fn launch(
    store: &Store,
    id: &RequestId,
    worker: &ProcessIdentity,
    generation: u64,
) -> Result<(ProcessIdentity, Child)> {
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--state")
        .arg(&store.path)
        .arg("--config")
        .arg(&store.config_path)
        .arg("watchdog")
        .arg(&id.0)
        .arg(generation.to_string())
        .process_group(0)
        .spawn()?;
    loop {
        let ready = store.transaction(|state, config| {
            let request = state.find_mut(id)?;
            if request.phase != Phase::Running
                || request.worker.as_ref() != Some(worker)
                || request.worker_generation != generation
            {
                return Err(Error::new(
                    ErrorCode::State,
                    "watchdog lease changed before startup",
                ));
            }
            let processes = Processes::read();
            if let Some(watchdog) = &request.watchdog
                && watchdog.pid == child.id()
                && processes.role_identity(watchdog, id, "watchdog", watchdog.pid)
                    == IdentityState::Matching
            {
                return Ok((Some(watchdog.clone()), config.poll_ms));
            }
            if request.started_clock_ms.is_some_and(|started| {
                clock_ms().is_ok_and(|now| now.saturating_sub(started) >= config.hold_ms)
            }) {
                request.stop = Some(StopReason::TimedOut);
                return Err(Error::new(
                    ErrorCode::State,
                    "watchdog startup exceeded hold deadline",
                ));
            }
            Ok((None, config.poll_ms))
        })?;
        if let Some(identity) = ready.0 {
            return Ok((identity, child));
        }
        if child.try_wait()?.is_some() {
            return Err(Error::new(
                ErrorCode::State,
                "watchdog exited before startup",
            ));
        }
        std::thread::sleep(Duration::from_millis(ready.1));
    }
}

pub(crate) fn run(store: &Store, id: &RequestId, generation: u64) -> Result<()> {
    let policy = Config::read(&store.config_path)?;
    let ignored = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, ignored.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, ignored)?;
    let worker = store.transaction(|state, _| {
        let request = state.find(id)?;
        if request.phase != Phase::Running || request.worker_generation != generation {
            return Err(Error::new(
                ErrorCode::State,
                "watchdog has no running lease",
            ));
        }
        request
            .worker
            .clone()
            .ok_or_else(|| Error::new(ErrorCode::State, "lease has no worker"))
    })?;
    let identity = Processes::read().current()?;
    let registered = store.transaction(|state, _| {
        let request = state.find_mut(id)?;
        if request.phase != Phase::Running
            || request.worker.as_ref() != Some(&worker)
            || request.worker_generation != generation
            || request.watchdog.is_some()
        {
            return Ok(false);
        }
        request.watchdog = Some(identity.clone());
        Ok(true)
    })?;
    if !registered {
        return Ok(());
    }
    let result = enforce(store, id, generation, &worker, &identity);
    if result.is_err() {
        terminate_group(id, &identity, identity.pid, &policy)?;
    }
    result
}

enum Decision {
    Done,
    Observe {
        stop: bool,
        memory_cap_exceeded: bool,
        grace_ms: u64,
        poll_ms: u64,
    },
}

fn observe(
    store: &Store,
    id: &RequestId,
    generation: u64,
    worker: &ProcessIdentity,
    watchdog: &ProcessIdentity,
    read_processes: impl FnOnce() -> Processes,
    probe_exit: impl FnOnce(u32) -> Result<bool>,
) -> Result<Decision> {
    store.transaction(|state, config| {
        let processes = read_processes();
        let request = state.find_mut(id)?;
        if request.phase.terminal()
            || request.worker.as_ref() != Some(worker)
            || request.worker_generation != generation
            || request.watchdog.as_ref() != Some(watchdog)
        {
            return Ok(Decision::Done);
        }
        if request.stop.is_none()
            && request.started_clock_ms.is_some_and(|started| {
                clock_ms().is_ok_and(|now| now.saturating_sub(started) >= config.hold_ms)
            })
        {
            request.stop = Some(StopReason::TimedOut);
        }
        if request.stop.is_none()
            && request
                .lease_record
                .as_ref()
                .and_then(|record| {
                    rss_fallback_cap(
                        record.memory_enforcement,
                        request.memory_cap_bytes,
                        config.memory_cap_bytes,
                    )
                })
                .is_some_and(|cap| {
                    processes
                        .group_memory_bytes(watchdog.pid, &[watchdog.pid])
                        .is_some_and(|used| used > cap)
                })
        {
            request.stop = Some(StopReason::MemoryCap);
        }
        let worker_state = processes.identity(worker, id);
        let worker_lost = matches!(
            worker_state,
            IdentityState::Absent | IdentityState::Different
        );
        let no_command_members =
            !processes.group_has_live_members(watchdog.pid, Some(watchdog.pid));
        if (worker_lost || request.command_finished.is_some())
            && no_command_members
            && probe_exit(watchdog.pid)?
        {
            let completion = match &request.command_finished {
                Some(outcome) => Completion {
                    code: outcome.code,
                    signal: outcome.signal,
                    reason: None,
                },
                None => Completion {
                    reason: Some("supervisor_lost"),
                    ..Completion::default()
                },
            };
            state.finish(id, completion, config, now_ms()?, clock_ms()?)?;
            return Ok(Decision::Done);
        }
        if worker_state != IdentityState::Matching {
            request.reason = Some("supervisor_lost_watchdog_active".to_owned());
        }
        Ok(Decision::Observe {
            stop: request.stop.is_some() || request.command_finished.is_some(),
            memory_cap_exceeded: request.stop == Some(StopReason::MemoryCap),
            grace_ms: config.termination_grace_ms,
            poll_ms: config.poll_ms,
        })
    })
}

fn rss_fallback_cap(
    enforcement: MemoryEnforcement,
    request_cap: Option<u64>,
    policy_cap: Option<u64>,
) -> Option<u64> {
    (enforcement == MemoryEnforcement::RssWatchdog)
        .then(|| request_cap.or(policy_cap))
        .flatten()
}

fn enforce(
    store: &Store,
    id: &RequestId,
    generation: u64,
    worker: &ProcessIdentity,
    watchdog: &ProcessIdentity,
) -> Result<()> {
    let mut stopping: Option<Instant> = None;
    loop {
        let decision = observe(
            store,
            id,
            generation,
            worker,
            watchdog,
            Processes::read,
            leave_and_verify_group_exit,
        )?;
        match decision {
            Decision::Done => return Ok(()),
            Decision::Observe {
                stop,
                memory_cap_exceeded,
                grace_ms,
                poll_ms,
            } => {
                if stop {
                    let processes = Processes::read();
                    if memory_cap_exceeded {
                        processes.signal_member_owned(
                            watchdog,
                            id,
                            "watchdog",
                            watchdog.pid,
                            Signal::SIGKILL,
                        )?;
                        stopping = Some(Instant::now());
                    } else if let Some(started) = stopping {
                        if started.elapsed() >= Duration::from_millis(grace_ms) {
                            processes.signal_member_owned(
                                watchdog,
                                id,
                                "watchdog",
                                watchdog.pid,
                                Signal::SIGKILL,
                            )?;
                        }
                    } else {
                        processes.signal_member_owned(
                            watchdog,
                            id,
                            "watchdog",
                            watchdog.pid,
                            Signal::SIGTERM,
                        )?;
                        stopping = Some(Instant::now());
                    }
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
            }
        }
    }
}

fn leave_and_verify_group_exit(group: u32) -> Result<bool> {
    // This process is the original group leader. Its still-live PID pins the
    // numeric group ID even while it is temporarily outside that group.
    if group != std::process::id() {
        return Err(Error::new(
            ErrorCode::State,
            "only the group leader can probe exit",
        ));
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("parking");
    probe_with_parking(group, &mut command)
}

fn probe_with_parking(group: u32, command: &mut Command) -> Result<bool> {
    let mut parking = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let probe = (|| {
        let parking_pid = i32::try_from(parking.id())
            .map_err(|_| Error::new(ErrorCode::State, "parking group out of range"))?;
        setpgid(Pid::this(), Pid::from_raw(parking_pid))
            .map_err(|error| Error::new(ErrorCode::Io, format!("leave group: {error}")))?;
        let absent = group_absent(group);
        if !absent {
            // A missed child, including a concurrently forked descendant, retains
            // occupancy. Rejoin the pinned group to keep enforcing its deadline.
            setpgid(Pid::this(), Pid::this())
                .map_err(|error| Error::new(ErrorCode::Io, format!("rejoin group: {error}")))?;
        }
        Ok(absent)
    })();
    drop(parking.stdin.take());
    parking.wait()?;
    probe
}

fn terminate_group(
    id: &RequestId,
    watchdog: &ProcessIdentity,
    group: u32,
    policy: &Config,
) -> Result<()> {
    Processes::read().signal_member_owned(watchdog, id, "watchdog", group, Signal::SIGTERM)?;
    let started = Instant::now();
    loop {
        let processes = Processes::read();
        if !processes.group_has_live_members(group, Some(watchdog.pid))
            && leave_and_verify_group_exit(group)?
        {
            return Ok(());
        }
        if started.elapsed() >= Duration::from_millis(policy.termination_grace_ms) {
            processes.signal_member_owned(watchdog, id, "watchdog", group, Signal::SIGKILL)?;
        }
        std::thread::sleep(Duration::from_millis(policy.poll_ms));
    }
}

#[cfg(test)]
mod tests;
