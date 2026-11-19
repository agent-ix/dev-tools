// SPDX-License-Identifier: MIT
//! Per-request durable supervisors; no daemon or inherited shell is required.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::sys::signal::Signal;
use serde::Serialize;

use crate::config::HostSample;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Completion, Event, Phase, Request, RequestId, StopReason};
use crate::process::{IdentityState, Processes};
use crate::store::Store;

pub(crate) fn now_ms() -> Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::new(ErrorCode::State, "system clock before Unix epoch"))?
        .as_millis();
    u64::try_from(millis).map_err(|_| Error::new(ErrorCode::State, "system clock out of range"))
}

pub(crate) fn clock_ms() -> Result<u64> {
    let time = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let seconds = u64::try_from(time.tv_sec)
        .map_err(|_| Error::new(ErrorCode::State, "monotonic clock out of range"))?;
    let nanos = u64::try_from(time.tv_nsec)
        .map_err(|_| Error::new(ErrorCode::State, "monotonic clock out of range"))?;
    seconds
        .checked_mul(1000)
        .and_then(|value| value.checked_add(nanos / 1_000_000))
        .ok_or_else(|| Error::new(ErrorCode::State, "monotonic clock overflow"))
}

pub(crate) fn enqueue(
    store: &Store,
    owner: &str,
    ticket: &str,
    units: u32,
    priority: u32,
    command: &[String],
) -> Result<RequestId> {
    if owner.is_empty()
        || owner.len() > 256
        || ticket.is_empty()
        || ticket.len() > 128
        || command.is_empty()
        || command.len() > 256
        || command.iter().any(|arg| arg.len() > 32_768)
    {
        return Err(Error::new(
            ErrorCode::Request,
            "invalid owner, ticket or command bounds",
        ));
    }
    let cwd = std::env::current_dir()?;
    let now = now_ms()?;
    let clock = clock_ms()?;
    store.transaction(|state, config| {
        if units == 0 || units > config.maximum_units || priority > config.maximum_priority {
            return Err(Error::new(
                ErrorCode::Request,
                "units or priority outside configured bounds",
            ));
        }
        if state.requests.len() >= config.maximum_requests {
            return Err(Error::new(
                ErrorCode::State,
                "retained request limit reached; prune terminal requests",
            ));
        }
        let sequence = state.next_sequence;
        state.next_sequence = sequence
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "request sequence exhausted"))?;
        let id = RequestId(format!("{}:r{sequence}", state.store_id));
        state.requests.push(Request {
            id: id.clone(),
            sequence,
            owner: owner.to_owned(),
            ticket: ticket.to_owned(),
            units,
            priority,
            enqueued_at_ms: now,
            enqueued_clock_ms: clock,
            started_at_ms: None,
            started_clock_ms: None,
            ended_at_ms: None,
            ended_clock_ms: None,
            heartbeat_at_ms: None,
            worker: None,
            phase: Phase::Queued,
            stop: None,
            exit_code: None,
            signal: None,
            reason: None,
            command: command.to_vec(),
            cwd,
        });
        state.event(&id, config, now, clock)?;
        Ok(id)
    })
}

pub(crate) fn spawn_worker(store: &Store, id: &RequestId) -> Result<()> {
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(store.path.join(format!("{}.stdout", id.0)))?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(store.path.join(format!("{}.stderr", id.0)))?;
    Command::new(std::env::current_exe()?)
        .arg("--state")
        .arg(&store.path)
        .arg("--config")
        .arg(&store.config_path)
        .arg("worker")
        .arg(&id.0)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()?;
    Ok(())
}

/// Redacted snapshot suitable for deterministic monitoring consumers.
#[derive(Serialize)]
pub struct Report {
    /// Report schema version.
    pub schema_version: u32,
    /// Stable store identity for event deduplication.
    pub store_id: String,
    /// Host identity.
    pub host: String,
    /// Observation UTC timestamp.
    pub observed_at_ms: u64,
    /// Configured currently enabled units.
    pub capacity_units: u32,
    /// Occupied units, including suspect leases.
    pub used_units: u32,
    /// Free admission units.
    pub free_units: u32,
    /// Running occupancy above reduced capacity.
    pub overcommit_units: u32,
    /// Host sample, absent when collection failed.
    pub sample: Option<HostSample>,
    /// Source collection reason, independent from queue state.
    pub source_error: Option<String>,
    /// Requests without command arguments or environment.
    pub requests: Vec<RequestReport>,
    /// Retained transition evidence; pruning is explicit.
    pub events: Vec<Event>,
}

/// One redacted request, including exact wait/hold ages and uncertainty.
#[derive(Serialize)]
pub struct RequestReport {
    /// Stable request identity.
    pub request_id: RequestId,
    /// Owner/lane, never inferred from a pane.
    pub owner: String,
    /// Owning ticket.
    pub ticket: String,
    /// State-machine phase.
    pub state: String,
    /// Lease units.
    pub units: u32,
    /// Base priority.
    pub priority: u32,
    /// Enqueue time.
    pub enqueued_at_ms: u64,
    /// Start time when admitted.
    pub started_at_ms: Option<u64>,
    /// Terminal time when complete.
    pub ended_at_ms: Option<u64>,
    /// Current or final queue duration.
    pub wait_ms: u64,
    /// Current or final hold duration.
    pub hold_ms: Option<u64>,
    /// True when ownership/heartbeat evidence is incomplete.
    pub suspect: bool,
    /// Reason for admission wait or final outcome.
    pub reason: Option<String>,
    /// Exact direct-child exit status when observed.
    pub exit_code: Option<i32>,
    /// Signal terminating the direct child when observed.
    pub signal: Option<i32>,
}

pub(crate) fn recover(store: &Store) -> Result<Vec<RequestId>> {
    let now = now_ms()?;
    let clock = clock_ms()?;
    let processes = Processes::read();
    store.transaction(|state, config| {
        let rebooted = state.boot_time != sysinfo::System::boot_time();
        let mut respawn = Vec::new();
        let mut finish = Vec::new();
        for request in &mut state.requests {
            if request.phase.terminal() {
                continue;
            }
            if rebooted {
                request.worker = None;
                if request.phase == Phase::Running {
                    finish.push(request.id.clone());
                } else {
                    request.enqueued_clock_ms = clock;
                }
            }
            let identity = request
                .worker
                .as_ref()
                .map(|identity| processes.identity(identity, &request.id));
            match request.phase {
                Phase::Queued => {
                    if matches!(
                        identity,
                        None | Some(IdentityState::Absent | IdentityState::Different)
                    ) {
                        request.worker = None;
                        respawn.push(request.id.clone());
                    }
                }
                Phase::Running => {
                    if rebooted
                        || request.worker.as_ref().is_some_and(|worker| {
                            matches!(
                                identity,
                                Some(IdentityState::Absent | IdentityState::Different)
                            ) && !processes.group_has_live_members(worker.pid, None)
                        })
                    {
                        finish.push(request.id.clone());
                    } else if identity != Some(IdentityState::Matching) {
                        request.reason = Some("supervisor_lost_group_exit_unverified".into());
                    }
                }
                Phase::Succeeded | Phase::Failed | Phase::TimedOut | Phase::Cancelled => {}
            }
        }
        finish.sort_by(|left, right| left.0.cmp(&right.0));
        finish.dedup();
        for id in finish {
            state.finish(
                &id,
                Completion {
                    code: None,
                    signal: None,
                    reason: Some(if rebooted {
                        "host_rebooted"
                    } else {
                        "supervisor_lost"
                    }),
                },
                config,
                now,
                clock,
            )?;
        }
        state.boot_time = sysinfo::System::boot_time();
        Ok(respawn)
    })
}

pub(crate) fn status(store: &Store) -> Result<Report> {
    let now = now_ms()?;
    let clock = clock_ms()?;
    let processes = Processes::read();
    store.transaction(|state, config| {
        let sample = config.sample(now);
        let (sample, source_error) = match sample {
            Ok(sample) => (Some(sample), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let requests = state
            .requests
            .iter()
            .map(|request| {
                let ended_clock = request.ended_clock_ms.unwrap_or(clock);
                let identity = request
                    .worker
                    .as_ref()
                    .map(|worker| processes.identity(worker, &request.id));
                RequestReport {
                    request_id: request.id.clone(),
                    owner: request.owner.clone(),
                    ticket: request.ticket.clone(),
                    state: phase_name(request.phase).to_owned(),
                    units: request.units,
                    priority: request.priority,
                    enqueued_at_ms: request.enqueued_at_ms,
                    started_at_ms: request.started_at_ms,
                    ended_at_ms: request.ended_at_ms,
                    wait_ms: request
                        .started_clock_ms
                        .unwrap_or(ended_clock)
                        .saturating_sub(request.enqueued_clock_ms),
                    hold_ms: request
                        .started_clock_ms
                        .map(|start| ended_clock.saturating_sub(start)),
                    suspect: request.phase == Phase::Running
                        && (identity != Some(IdentityState::Matching)
                            || request.heartbeat_at_ms.is_none_or(|heartbeat| {
                                now.saturating_sub(heartbeat) > config.heartbeat_max_age_ms
                            })),
                    reason: request.reason.clone(),
                    exit_code: request.exit_code,
                    signal: request.signal,
                }
            })
            .collect();
        let used = state.used();
        Ok(Report {
            schema_version: 1,
            store_id: state.store_id.clone(),
            host: state.host.clone(),
            observed_at_ms: now,
            capacity_units: config.capacity_units,
            used_units: used,
            free_units: config.capacity_units.saturating_sub(used),
            overcommit_units: used.saturating_sub(config.capacity_units),
            sample,
            source_error,
            requests,
            events: state.events.clone(),
        })
    })
}

pub(crate) fn cancel(store: &Store, id: &RequestId) -> Result<()> {
    store.transaction(|state, config| {
        let request = state.find_mut(id)?;
        if request.phase.terminal() {
            return Ok(());
        }
        request.stop = Some(StopReason::Cancelled);
        if request.phase == Phase::Queued {
            state.finish(id, Completion::default(), config, now_ms()?, clock_ms()?)?;
        }
        Ok(())
    })
}

pub(crate) fn wait(store: &Store, id: &RequestId) -> Result<i32> {
    loop {
        for missing in recover(store)? {
            spawn_worker(store, &missing)?;
        }
        let result = store.transaction(|state, config| {
            let request = state.find(id)?;
            let code = match request.phase {
                Phase::Queued | Phase::Running => None,
                Phase::TimedOut => Some(124),
                Phase::Cancelled => Some(130),
                Phase::Succeeded => Some(0),
                Phase::Failed => Some(
                    request
                        .exit_code
                        .filter(|code| *code != 0)
                        .or_else(|| request.signal.map(|signal| 128_i32.saturating_add(signal)))
                        .unwrap_or(1),
                ),
            };
            Ok((code, config.poll_ms))
        })?;
        if let Some(code) = result.0 {
            return Ok(code);
        }
        std::thread::sleep(Duration::from_millis(result.1));
    }
}

pub(crate) fn worker(store: &Store, id: &RequestId) -> Result<()> {
    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, interrupted.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())?;
    let identity = Processes::read().current()?;
    let claimed = store.transaction(|state, _| {
        let request = state.find_mut(id)?;
        if request.phase != Phase::Queued || request.worker.is_some() {
            return Ok(false);
        }
        request.worker = Some(identity.clone());
        Ok(true)
    })?;
    if !claimed {
        return Ok(());
    }
    let request = loop {
        let now = now_ms()?;
        let clock = clock_ms()?;
        let decision = store.transaction(|state, config| {
            if state.find(id)?.phase != Phase::Queued {
                return Ok((None, true, config.poll_ms));
            }
            if interrupted.load(Ordering::Relaxed) {
                state.find_mut(id)?.stop = Some(StopReason::Cancelled);
                state.finish(id, Completion::default(), config, now, clock)?;
                return Ok((None, true, config.poll_ms));
            }
            let sample = config.sample(now).ok();
            let reason = state.admission_reason(id, config, sample.as_ref(), now, clock)?;
            let request = state.find_mut(id)?;
            request.heartbeat_at_ms = Some(now);
            request.reason = reason.map(str::to_owned);
            if reason.is_some() {
                return Ok((None, false, config.poll_ms));
            }
            request.phase = Phase::Running;
            request.started_at_ms = Some(now);
            request.started_clock_ms = Some(clock);
            let admitted = request.clone();
            state.event(id, config, now, clock)?;
            Ok((Some(admitted), false, config.poll_ms))
        })?;
        if let Some(request) = decision.0 {
            break request;
        }
        if decision.1 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(decision.2));
    };
    let cleanup_policy = crate::config::Config::read(&store.config_path)?;
    let result = supervise(store, &request, &identity, &interrupted);
    if result.is_err() {
        let processes = Processes::read();
        if processes.group_has_live_members(identity.pid, Some(identity.pid)) {
            processes.signal_owned(&identity, id, Signal::SIGTERM)?;
            let deadline = Instant::now()
                .checked_add(Duration::from_millis(cleanup_policy.termination_grace_ms))
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::Configuration,
                        "termination grace exceeds clock range",
                    )
                })?;
            loop {
                let processes = Processes::read();
                if !processes.group_has_live_members(identity.pid, Some(identity.pid)) {
                    break;
                }
                if Instant::now() >= deadline {
                    processes.signal_owned(&identity, id, Signal::SIGKILL)?;
                }
                std::thread::sleep(Duration::from_millis(cleanup_policy.poll_ms));
            }
        }
    }
    result
}

fn supervise(
    store: &Store,
    request: &Request,
    identity: &crate::model::ProcessIdentity,
    interrupted: &AtomicBool,
) -> Result<()> {
    let Some(program) = request.command.first() else {
        return Err(Error::new(ErrorCode::State, "persisted empty command"));
    };
    let mut child = match Command::new(program)
        .args(request.command.iter().skip(1))
        .current_dir(&request.cwd)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return store.transaction(|state, config| {
                state.finish(
                    &request.id,
                    Completion {
                        reason: Some("command_spawn_failed"),
                        ..Completion::default()
                    },
                    config,
                    now_ms()?,
                    clock_ms()?,
                )
            });
        }
    };
    let started = Instant::now();
    let mut stopping: Option<Instant> = None;
    let mut status: Option<ExitStatus> = None;
    loop {
        let policy = store.transaction(|state, config| {
            let current = state.find_mut(&request.id)?;
            current.heartbeat_at_ms = Some(now_ms()?);
            if current.stop.is_none() {
                if interrupted.load(Ordering::Relaxed) && stopping.is_none() {
                    current.stop = Some(StopReason::Cancelled);
                } else if started.elapsed() >= Duration::from_millis(config.hold_ms) {
                    current.stop = Some(StopReason::TimedOut);
                }
            }
            Ok((current.stop, config.termination_grace_ms, config.poll_ms))
        })?;
        if status.is_none() {
            status = child.try_wait()?;
        }
        let processes = Processes::read();
        let has_descendants = processes.group_has_live_members(identity.pid, Some(identity.pid));
        if status.is_some() && !has_descendants {
            return store.transaction(|state, config| {
                state.finish(
                    &request.id,
                    Completion {
                        code: status.and_then(|value| value.code()),
                        signal: status.and_then(|value| value.signal()),
                        reason: stopping.map(|_| "remaining_descendants_terminated"),
                    },
                    config,
                    now_ms()?,
                    clock_ms()?,
                )
            });
        }
        if policy.0.is_some()
            || (status.is_some()
                && processes.confirmed_group_has_live_members(identity.pid, Some(identity.pid)))
        {
            if let Some(stopping_at) = stopping {
                if stopping_at.elapsed() >= Duration::from_millis(policy.1) {
                    // SIGKILL includes this stable group leader. Recovery finalizes only after
                    // all live members disappear, retaining the lease across supervisor death.
                    processes.signal_owned(identity, &request.id, Signal::SIGKILL)?;
                }
            } else {
                processes.signal_owned(identity, &request.id, Signal::SIGTERM)?;
                stopping = Some(Instant::now());
            }
        }
        std::thread::sleep(Duration::from_millis(policy.2));
    }
}

pub(crate) fn prune(store: &Store) -> Result<usize> {
    store.transaction(|state, _| {
        let original = state.requests.len();
        let terminal: Vec<_> = state
            .requests
            .iter()
            .filter(|request| request.phase.terminal())
            .map(|request| request.id.clone())
            .collect();
        state.requests.retain(|request| !request.phase.terminal());
        state
            .events
            .retain(|event| !terminal.contains(&event.request_id));
        Ok(original.saturating_sub(state.requests.len()))
    })
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Queued => "queued",
        Phase::Running => "running",
        Phase::Succeeded => "succeeded",
        Phase::Failed => "failed",
        Phase::TimedOut => "timed_out",
        Phase::Cancelled => "cancelled",
    }
}

pub(crate) fn open(state: &Path, config: &Path) -> Result<Store> {
    Store::open(state, config)
}
