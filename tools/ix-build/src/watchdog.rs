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
    let mut command = startup_command(store, id, generation)?;
    let mut child = command.spawn()?;
    loop {
        let ready = match store.transaction(|state, config| {
            let request = state.find_mut(id)?;
            if request.phase != Phase::Running
                || request.worker.as_ref() != Some(worker)
                || request.worker_generation != generation
            {
                return Err(Error::new(
                    ErrorCode::State,
                    format!("watchdog lease changed before startup: request={} state={} phase={:?} expected_worker={} expected_generation={} actual_worker={:?} actual_generation={}", id.0, store.path.display(), request.phase, worker.pid, generation, request.worker.as_ref().map(|identity| identity.pid), request.worker_generation),
                ));
            }
            if let Some(watchdog) = &request.watchdog
                && watchdog.pid == child.id()
                && !crate::process::owned_child_exited(&child)?
            {
                return Ok((Some(watchdog.clone()), crate::config::POLL_INTERVAL_MS));
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
            Ok((None, crate::config::POLL_INTERVAL_MS))
        }) {
            Ok(ready) => ready,
            Err(error) => {
                let group = child.id();
                let grace = Config::read(&store.config_path).map(|config| config.termination_grace_ms).unwrap_or(10_000);
                crate::runtime::terminate_supervision_group(&mut child, None, group, grace)?;
                eprintln!("request={} reason=supervision_lost detail={error}", id.0);
                return Err(error);
            }
        };
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

fn startup_command(store: &Store, id: &RequestId, generation: u64) -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--state")
        .arg(&store.path)
        .arg("--config")
        .arg(&store.config_path)
        .arg("watchdog")
        .arg(&id.0)
        .arg(generation.to_string())
        .stdin(Stdio::null())
        .process_group(0);
    Ok(command)
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
    if let Err(error) = &result {
        // Publish the actual signal's source before terminating the group.
        // The worker must not classify our cleanup as an outside SIGTERM.
        if let Err(record_error) = store.transaction(|state, _| {
            let request = state.find_mut(id)?;
            if request.phase == Phase::Running
                && request.worker_generation == generation
                && request.watchdog.as_ref() == Some(&identity)
            {
                request.reason = Some("supervision_lost".to_owned());
                request.supervision_detail = Some(error.message.to_string());
            }
            Ok(())
        }) {
            eprintln!("ix-build: supervision-loss evidence unavailable: {record_error}");
        }
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
    observe_with_clock(
        store,
        id,
        generation,
        worker,
        watchdog,
        || Ok((now_ms()?, clock_ms()?)),
        read_processes,
        probe_exit,
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn observe_at(
    store: &Store,
    id: &RequestId,
    generation: u64,
    worker: &ProcessIdentity,
    watchdog: &ProcessIdentity,
    now: u64,
    clock: u64,
    read_processes: impl FnOnce() -> Processes,
    probe_exit: impl FnOnce(u32) -> Result<bool>,
) -> Result<Decision> {
    observe_with_clock(
        store,
        id,
        generation,
        worker,
        watchdog,
        || Ok((now, clock)),
        read_processes,
        probe_exit,
    )
}

#[allow(clippy::too_many_arguments)]
fn observe_with_clock(
    store: &Store,
    id: &RequestId,
    generation: u64,
    worker: &ProcessIdentity,
    watchdog: &ProcessIdentity,
    read_clock: impl FnMut() -> Result<(u64, u64)>,
    read_processes: impl FnOnce() -> Processes,
    probe_exit: impl FnOnce(u32) -> Result<bool>,
) -> Result<Decision> {
    store.transaction_with_clock(read_clock, |state, config, now, clock| {
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
            && request
                .started_clock_ms
                .is_some_and(|started| clock.saturating_sub(started) >= config.hold_ms)
        {
            request.stop = Some(StopReason::TimedOut);
        }
        if request.stop.is_none() {
            let cap = request.lease_record.as_ref().and_then(|record| {
                rss_fallback_cap(record.memory_enforcement, request.memory_cap_bytes)
            });
            if let Some(cap) = cap {
                let sample = request
                    .lease_record
                    .as_ref()
                    .and_then(|record| record.command.as_ref())
                    .and_then(|root| processes.command_memory_sample(root));
                if let Some(sample) = sample {
                    observe_memory_sample(request, sample, cap);
                } else if processes.group_has_live_members(watchdog.pid, Some(watchdog.pid)) {
                    return Err(Error::new(
                        ErrorCode::State,
                        "command-tree memory supervision unavailable",
                    ));
                }
            }
        }
        let worker_state = processes.identity(worker, id);
        let worker_lost = matches!(
            worker_state,
            IdentityState::Absent | IdentityState::Different
        );
        let no_command_members =
            !processes.group_has_live_members(watchdog.pid, Some(watchdog.pid));
        // The worker may still be polling a launch that never happened after
        // its original client disappeared. Fence that launch in this same
        // transaction; a live worker alone is not evidence of live work.
        let command_absent = request
            .lease_record
            .as_ref()
            .and_then(|record| record.command.as_ref())
            .map_or(request.command_started_clock_ms.is_none(), |root| {
                processes.birth_identity_absent(root) && crate::process::group_absent(root.pid)
            });
        let abandoned_launch = command_absent
            && request.client_pid_namespace == crate::process::pid_namespace()?
            && request
                .client
                .as_ref()
                .is_some_and(|client| processes.birth_identity_absent(client));
        if (worker_lost || abandoned_launch || request.command_finished.is_some())
            && no_command_members
            && probe_exit(watchdog.pid)?
        {
            if abandoned_launch {
                request.supervision_detail =
                    Some("original client and command are absent; worker fenced".into());
            }
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
            state.finish(id, completion, config, now, clock)?;
            return Ok(Decision::Done);
        }
        if worker_state != IdentityState::Matching {
            request.reason = Some("supervisor_lost_watchdog_active".to_owned());
        }
        Ok(Decision::Observe {
            stop: request.stop.is_some() || request.command_finished.is_some(),
            memory_cap_exceeded: request.stop == Some(StopReason::MemoryCap),
            grace_ms: config.termination_grace_ms,
            poll_ms: crate::config::POLL_INTERVAL_MS,
        })
    })
}

pub(crate) fn observe_memory_sample(
    request: &mut crate::model::Request,
    sample: crate::model::MemorySample,
    cap: u64,
) {
    request.measured_memory_bytes = Some(sample.bytes);
    if let Some(record) = &mut request.lease_record {
        if record.memory_sample.is_none() {
            let contributions = sample
                .contributors
                .iter()
                .map(|row| format!("{}:{}", row.pid, row.bytes))
                .collect::<Vec<_>>()
                .join(",");
            eprintln!(
                "request={} memory_sample metric={} measured_bytes={} memory_cap_bytes={cap} pids={contributions}",
                request.id.0,
                sample.metric.as_str(),
                sample.bytes
            );
            record.memory_sample = Some(sample.clone());
        }
        if record
            .memory_sample
            .as_ref()
            .is_none_or(|old| sample.bytes > old.bytes)
        {
            record.memory_sample = Some(sample.clone());
        }
    }
    if sample.bytes > cap {
        request.stop = Some(StopReason::MemoryCap);
    }
}

fn rss_fallback_cap(enforcement: MemoryEnforcement, request_cap: Option<u64>) -> Option<u64> {
    if enforcement == MemoryEnforcement::RssWatchdog {
        request_cap
    } else {
        None
    }
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
        std::thread::sleep(Duration::from_millis(crate::config::POLL_INTERVAL_MS));
    }
}

#[cfg(test)]
mod tests;
