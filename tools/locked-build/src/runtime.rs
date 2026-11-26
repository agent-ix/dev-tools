// SPDX-License-Identifier: MIT
//! Per-request durable supervisors; no daemon or inherited shell is required.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config::HostSample;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{CommandOutcome, Completion, Event, Phase, Request, RequestId, StopReason};
use crate::process::{IdentityState, Processes, group_absent};
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
            worker_generation: 0,
            watchdog: None,
            command_finished: None,
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

pub(crate) fn priority_for_class(store: &Store, class: &str) -> Result<u32> {
    store.transaction(|_, config| {
        config
            .priority_classes
            .get(class)
            .copied()
            .ok_or_else(|| Error::new(ErrorCode::Request, "priority class is not configured"))
    })
}

pub(crate) fn spawn_worker(store: &Store, id: &RequestId) -> Result<Child> {
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
    let child = Command::new(std::env::current_exe()?)
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
    Ok(child)
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
    /// Number of transitions removed by explicit pruning.
    pub pruned_events: u64,
    /// Next stable event sequence; permits detection of missed history.
    pub next_event_sequence: u64,
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
    recover_with_processes(store, Processes::read)
}

fn recover_with_processes(
    store: &Store,
    read_processes: impl FnOnce() -> Processes,
) -> Result<Vec<RequestId>> {
    store.transaction(|state, config| {
        let now = now_ms()?;
        let clock = clock_ms()?;
        let processes = read_processes();
        let rebooted = state.boot_time != sysinfo::System::boot_time();
        if rebooted {
            let new_boot_ms = sysinfo::System::boot_time().saturating_mul(1_000);
            for request in state
                .requests
                .iter()
                .filter(|request| request.phase == Phase::Queued)
            {
                state.metrics.record_pre_reboot_wait(
                    &request.id.0,
                    new_boot_ms.saturating_sub(request.enqueued_at_ms),
                );
            }
        }
        let mut respawn = Vec::new();
        let mut finish = Vec::new();
        for request in &mut state.requests {
            if request.phase.terminal() {
                continue;
            }
            if rebooted {
                request.worker = None;
                request.watchdog = None;
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
                            ) && group_absent(worker.pid)
                                && request
                                    .watchdog
                                    .as_ref()
                                    .is_none_or(|watchdog| group_absent(watchdog.pid))
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
            let outcome = state.find(&id)?.command_finished.as_ref();
            let code = outcome.and_then(|outcome| outcome.code);
            let signal = outcome.and_then(|outcome| outcome.signal);
            state.finish(
                &id,
                Completion {
                    code,
                    signal,
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
    store.transaction(|state, config| {
        let now = now_ms()?;
        let clock = clock_ms()?;
        let processes = Processes::read();
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
            pruned_events: state.pruned_events,
            next_event_sequence: state.next_event,
        })
    })
}

pub(crate) fn metrics(store: &Store) -> Result<String> {
    store.transaction(|state, config| {
        let now = now_ms()?;
        let clock = clock_ms()?;
        state.metrics.checkpoint_active(clock, now)?;
        state.metrics.exposition(state, config, now, clock)
    })
}

pub(crate) fn metrics_to_file(store: &Store, path: &Path) -> Result<()> {
    let _export_lock = store.lock_metrics_export()?;
    let text = metrics(store)?;
    crate::metrics::write_atomic(path, &text)
}

pub(crate) fn current_block_reason(
    state: &crate::model::State,
    config: &crate::config::Config,
    sample: Option<&HostSample>,
    now: u64,
    clock: u64,
) -> Result<Option<&'static str>> {
    let Some(next) = state.next(config, clock) else {
        return Ok(None);
    };
    let reason = state.admission_reason(&next.id, config, sample, now, clock)?;
    Ok(reason)
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

pub(crate) fn wait(store: &Store, id: &RequestId, mut workers: Vec<Child>) -> Result<i32> {
    loop {
        let mut active = Vec::new();
        for mut worker in workers {
            if worker.try_wait()?.is_none() {
                active.push(worker);
            }
        }
        workers = active;
        for missing in recover(store)? {
            workers.push(spawn_worker(store, &missing)?);
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

struct WorkerFence {
    identity: crate::model::ProcessIdentity,
    generation: u64,
}

impl WorkerFence {
    fn owns(&self, request: &Request) -> bool {
        request.worker.as_ref() == Some(&self.identity)
            && request.worker_generation == self.generation
    }

    fn require_running(&self, request: &Request) -> Result<()> {
        if !self.owns(request) || request.phase != Phase::Running {
            return Err(Error::new(
                ErrorCode::State,
                "supervisor lease ownership changed",
            ));
        }
        Ok(())
    }
}

fn claim_worker(
    store: &Store,
    id: &RequestId,
    identity: &crate::model::ProcessIdentity,
) -> Result<Option<WorkerFence>> {
    store.transaction(|state, _| {
        let request = state.find_mut(id)?;
        if request.phase != Phase::Queued || request.worker.is_some() {
            return Ok(None);
        }
        request.worker_generation = request
            .worker_generation
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "supervisor generation exhausted"))?;
        request.worker = Some(identity.clone());
        Ok(Some(WorkerFence {
            identity: identity.clone(),
            generation: request.worker_generation,
        }))
    })
}

enum Admission {
    Stopped,
    Waiting(u64),
    Granted(Box<Request>),
}

fn admit_worker(
    store: &Store,
    id: &RequestId,
    fence: &WorkerFence,
    interrupted: &AtomicBool,
) -> Result<Admission> {
    store.transaction(|state, config| {
        let now = now_ms()?;
        let clock = clock_ms()?;
        let request = state.find(id)?;
        if request.phase != Phase::Queued || !fence.owns(request) {
            return Ok(Admission::Stopped);
        }
        if interrupted.load(Ordering::Relaxed) {
            state.find_mut(id)?.stop = Some(StopReason::Cancelled);
            state.finish(id, Completion::default(), config, now, clock)?;
            return Ok(Admission::Stopped);
        }
        let sample = config.sample(now).ok();
        let reason = state.admission_reason(id, config, sample.as_ref(), now, clock)?;
        {
            let request = state.find_mut(id)?;
            request.heartbeat_at_ms = Some(now);
            request.reason = reason.map(str::to_owned);
        }
        let blocked = current_block_reason(state, config, sample.as_ref(), now, clock)?;
        state.metrics.observe_block(blocked, now, clock)?;
        if reason.is_some() {
            return Ok(Admission::Waiting(config.poll_ms));
        }
        let admitted = {
            let request = state.find_mut(id)?;
            request.phase = Phase::Running;
            request.started_at_ms = Some(now);
            request.started_clock_ms = Some(clock);
            request.clone()
        };
        state.event(id, config, now, clock)?;
        Ok(Admission::Granted(Box::new(admitted)))
    })
}

fn finish_owned(
    state: &mut crate::model::State,
    config: &crate::config::Config,
    id: &RequestId,
    fence: &WorkerFence,
    group: u32,
    completion: Completion<'_>,
) -> Result<()> {
    fence.require_running(state.find(id)?)?;
    if !group_absent(group) {
        return Err(Error::new(
            ErrorCode::State,
            "command group exit unverified",
        ));
    }
    state.finish(id, completion, config, now_ms()?, clock_ms()?)
}

pub(crate) fn worker(store: &Store, id: &RequestId) -> Result<()> {
    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, interrupted.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())?;
    let identity = Processes::read().current()?;
    let Some(fence) = claim_worker(store, id, &identity)? else {
        return Ok(());
    };
    let request = loop {
        match admit_worker(store, id, &fence, &interrupted)? {
            Admission::Stopped => return Ok(()),
            Admission::Waiting(poll_ms) => std::thread::sleep(Duration::from_millis(poll_ms)),
            Admission::Granted(request) => break *request,
        }
    };
    let (watchdog, mut watchdog_child) =
        crate::watchdog::launch(store, id, &identity, fence.generation)?;
    supervise(
        store,
        &request,
        &fence,
        &watchdog,
        &mut watchdog_child,
        &interrupted,
    )?;
    cleanup_target(store, id)
}

fn supervise(
    store: &Store,
    request: &Request,
    fence: &WorkerFence,
    watchdog: &crate::model::ProcessIdentity,
    watchdog_child: &mut Child,
    interrupted: &AtomicBool,
) -> Result<()> {
    let mut child = start_command(store, request, fence, watchdog)?;
    let mut status = None;
    loop {
        // A dead watchdog remains a group member until its parent reaps it.
        watchdog_child.try_wait()?;
        if status.is_none() {
            status = child.try_wait()?;
        }
        let policy = store.transaction(|state, config| {
            if state.find(&request.id)?.phase.terminal() {
                return Ok(None);
            }
            fence.require_running(state.find(&request.id)?)?;
            let current = state.find_mut(&request.id)?;
            if let Some(outcome) = status {
                current.command_finished = Some(CommandOutcome {
                    code: outcome.code(),
                    signal: outcome.signal(),
                });
            } else if Processes::read().role_identity(
                watchdog,
                &request.id,
                "watchdog",
                watchdog.pid,
            ) != IdentityState::Matching
            {
                return Err(Error::new(
                    ErrorCode::State,
                    "watchdog supervision unavailable",
                ));
            }
            current.heartbeat_at_ms = Some(now_ms()?);
            if current.stop.is_none() {
                if interrupted.load(Ordering::Relaxed) {
                    current.stop = Some(StopReason::Cancelled);
                } else if current.started_clock_ms.is_some_and(|start| {
                    clock_ms().is_ok_and(|now| now.saturating_sub(start) >= config.hold_ms)
                }) {
                    current.stop = Some(StopReason::TimedOut);
                }
            }
            if let Some(outcome) = status
                && group_absent(watchdog.pid)
            {
                finish_owned(
                    state,
                    config,
                    &request.id,
                    fence,
                    watchdog.pid,
                    Completion {
                        code: outcome.code(),
                        signal: outcome.signal(),
                        reason: None,
                    },
                )?;
                return Ok(None);
            }
            Ok(Some(config.poll_ms))
        })?;
        let Some(poll_ms) = policy else {
            return Ok(());
        };
        std::thread::sleep(Duration::from_millis(poll_ms));
    }
}

fn start_command(
    store: &Store,
    request: &Request,
    fence: &WorkerFence,
    watchdog: &crate::model::ProcessIdentity,
) -> Result<Child> {
    store.transaction(|state, config| {
        fence.require_running(state.find(&request.id)?)?;
        if state.find(&request.id)?.watchdog.as_ref() != Some(watchdog)
            || Processes::read().role_identity(watchdog, &request.id, "watchdog", watchdog.pid)
                != IdentityState::Matching
        {
            return Err(Error::new(
                ErrorCode::State,
                "command group ownership unavailable",
            ));
        }
        let program = request
            .command
            .first()
            .ok_or_else(|| Error::new(ErrorCode::State, "persisted empty command"))?;
        let group = i32::try_from(watchdog.pid)
            .map_err(|_| Error::new(ErrorCode::State, "command group out of range"))?;
        // Creation and every quiescence decision share the durable store lock.
        let active_units = state
            .requests
            .iter()
            .filter(|candidate| candidate.phase == Phase::Running)
            .fold(0_u32, |total, candidate| {
                total.saturating_add(candidate.units)
            });
        let mut command = Command::new(program);
        command
            .args(request.command.iter().skip(1))
            .current_dir(&request.cwd)
            .process_group(group);
        apply_build_environment(&mut command, config, active_units);
        apply_target_environment(&mut command, config, request)?;
        command.spawn().map_err(Into::into)
    })
}

fn apply_build_environment(
    command: &mut Command,
    config: &crate::config::Config,
    active_units: u32,
) {
    if let Some(maximum_jobs) = config.cargo_build_jobs {
        let pressure_units = active_units.max(1);
        command.env(
            "CARGO_BUILD_JOBS",
            (maximum_jobs / pressure_units).max(1).to_string(),
        );
    }
    if let Some(threads) = config.rust_test_threads {
        command.env("RUST_TEST_THREADS", threads.to_string());
    }
    if let Some(incremental) = config.cargo_incremental {
        command.env("CARGO_INCREMENTAL", if incremental { "1" } else { "0" });
    }
}

fn target_directory(
    config: &crate::config::Config,
    request: &Request,
    create: bool,
) -> Result<Option<std::path::PathBuf>> {
    let Some(configured_root) = &config.target_root else {
        return Ok(None);
    };
    if create {
        std::fs::create_dir_all(configured_root)?;
    } else if !configured_root.exists() {
        return Ok(None);
    }
    let root = configured_root.canonicalize()?;
    let target = root.join(owner_target_key(&request.owner));
    if create {
        match std::fs::create_dir(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::new(
            ErrorCode::Configuration,
            "target directory is not a regular directory",
        ));
    }
    let canonical_target = target.canonicalize()?;
    if !canonical_target.starts_with(&root) {
        return Err(Error::new(
            ErrorCode::Configuration,
            "target directory escapes target_root",
        ));
    }
    Ok(Some(canonical_target))
}

fn apply_target_environment(
    command: &mut Command,
    config: &crate::config::Config,
    request: &Request,
) -> Result<()> {
    if let Some(target) = target_directory(config, request, true)? {
        command.env("CARGO_TARGET_DIR", target);
    }
    Ok(())
}

fn owner_target_key(owner: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in owner.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let prefix: String = owner
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || "-_.".contains(*character))
        .take(48)
        .collect();
    format!(
        "{}-{hash:016x}",
        if prefix.is_empty() { "owner" } else { &prefix }
    )
}

fn cleanup_target(store: &Store, id: &RequestId) -> Result<()> {
    store.transaction(|state, config| {
        let request = state.find(id)?;
        let still_active = state.requests.iter().any(|candidate| {
            candidate.id != request.id
                && candidate.owner == request.owner
                && candidate.phase == Phase::Running
        });
        if still_active {
            return Ok(());
        }
        let Some(target) = target_directory(config, request, false)? else {
            return Ok(());
        };
        let Some((bytes, newest_modified)) = inspect_target_tree(&target)? else {
            return Ok(());
        };
        let too_large = config.max_bytes.is_some_and(|limit| bytes > limit);
        let too_old = config.trim_after_ms.is_some_and(|limit| {
            std::time::SystemTime::now()
                .duration_since(newest_modified)
                .is_ok_and(|age| age.as_millis() >= u128::from(limit))
        });
        if too_large || too_old {
            let confirmed = target_directory(config, request, false)?;
            if confirmed.as_deref() != Some(target.as_path()) {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "target directory changed before cleanup",
                ));
            }
            std::fs::remove_dir_all(target)?;
        }
        Ok(())
    })
}

fn inspect_target_tree(target: &Path) -> Result<Option<(u64, std::time::SystemTime)>> {
    let root_metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(Error::new(
            ErrorCode::Configuration,
            "target directory is not a regular directory",
        ));
    }
    let mut pending = vec![target.to_owned()];
    let mut bytes = 0_u64;
    let mut newest_modified = root_metadata.modified()?;
    let mut visited = 0_usize;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            visited = visited.saturating_add(1);
            if visited > 100_000 {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "target directory contains too many entries to inspect",
                ));
            }
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "target directory contains a symlink",
                ));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
            }
            newest_modified = newest_modified.max(metadata.modified()?);
        }
    }
    Ok(Some((bytes, newest_modified)))
}

pub(crate) fn prune(store: &Store) -> Result<usize> {
    let terminal = store.transaction(|state, _| {
        let terminal: Vec<_> = state
            .requests
            .iter()
            .filter(|request| request.phase.terminal())
            .map(|request| request.id.clone())
            .collect();
        state.requests.retain(|request| !request.phase.terminal());
        let previous_events = state.events.len();
        state
            .events
            .retain(|event| !terminal.contains(&event.request_id));
        state.pruned_events = state
            .pruned_events
            .checked_add(
                u64::try_from(previous_events.saturating_sub(state.events.len()))
                    .map_err(|_| Error::new(ErrorCode::State, "pruned event count out of range"))?,
            )
            .ok_or_else(|| Error::new(ErrorCode::State, "pruned event count overflow"))?;
        Ok(terminal)
    })?;
    for id in &terminal {
        for extension in ["stdout", "stderr"] {
            match std::fs::remove_file(store.path.join(format!("{}.{extension}", id.0))) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(terminal.len())
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

#[cfg(test)]
mod tests;
