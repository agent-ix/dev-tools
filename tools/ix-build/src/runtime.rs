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

use crate::config::{Config, HostSample};
use crate::error::{Error, ErrorCode, Result};
use crate::model::{
    CommandOutcome, Completion, Event, LeaseRecord, MemoryEnforcement, Phase, Request, RequestId,
    StopReason,
};
use crate::process::{IdentityState, Processes, group_absent};
use crate::scope_properties::ScopeProperties;
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

#[cfg(test)]
pub(crate) fn enqueue(
    store: &Store,
    owner: &str,
    ticket: &str,
    units: u32,
    priority: u32,
    class: Option<&str>,
    command: &[String],
) -> Result<RequestId> {
    enqueue_with_memory(store, owner, ticket, units, priority, class, command, None)
}

#[allow(clippy::too_many_arguments)]
/// Provenance: SWM-437. Trace: FR-005-AC-4, FR-015-AC-8
pub(crate) fn enqueue_with_memory(
    store: &Store,
    owner: &str,
    ticket: &str,
    units: u32,
    priority: u32,
    class: Option<&str>,
    command: &[String],
    memory: Option<u64>,
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
    let telemetry = crate::telemetry::EventTelemetry::identity(&cwd, command);
    let caller_cargo_build_jobs = std::env::var("CARGO_BUILD_JOBS").ok();
    let caller_rust_test_threads = std::env::var("RUST_TEST_THREADS").ok();
    let caller_cargo_incremental = std::env::var("CARGO_INCREMENTAL").ok();
    let caller_cargo_target_dir = std::env::var("CARGO_TARGET_DIR").ok();
    validate_parallelism_input(
        command,
        caller_cargo_build_jobs.as_deref(),
        caller_rust_test_threads.as_deref(),
    )?;
    let now = now_ms()?;
    let clock = clock_ms()?;
    store.transaction(|state, config| {
        let sequence = state.next_sequence;
        state.next_sequence = sequence
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "request sequence exhausted"))?;
        let id = RequestId(format!("{}:r{sequence}", state.store_id));
        let class_name = class.unwrap_or("periodic");
        let mut attempt_trace = requested_trace(
            config,
            class_name,
            command,
            caller_cargo_build_jobs.as_deref(),
            caller_rust_test_threads.as_deref(),
            units,
            state.used().saturating_add(units),
        );
        if let Some(bytes) = memory {
            attempt_trace.requested_memory_bytes = Some(bytes);
            attempt_trace.requested_memory_reservation_bytes = Some(bytes);
        }
        let result = (|| {
            if let Some(bytes) = memory {
                let cap = config.request_memory_cap().unwrap_or(0);
                if bytes == 0 || bytes > cap || config.memory_admission != crate::config::MemoryAdmissionMode::LeaseAccounted {
                    return Err(Error::new(ErrorCode::Request, "explicit memory requires lease accounting and must fit policy.max_request_memory")
                        .with_failure(crate::diagnostics::Failure::refused(ix_build::wire::Code::RefusedClassCap, class_name, "policy.max_request_memory", cap.into(), bytes.into(), "requested memory bytes")));
                }
                if let Some(cap) = config.memory_cap_by_class.get(class_name).filter(|cap| bytes > **cap) {
                    return Err(Error::new(ErrorCode::Request, "explicit memory exceeds class cap")
                        .with_failure(crate::diagnostics::Failure::refused(ix_build::wire::Code::RefusedClassCap, class_name, &format!("policy.memory_cap_by_class.{class_name}"), (*cap).into(), bytes.into(), "requested memory bytes")));
                }
                if let Some(budget) = config.memory_budget_bytes.filter(|budget| bytes > *budget) {
                    return Err(Error::new(ErrorCode::Request, "explicit memory exceeds admission budget")
                        .with_failure(crate::diagnostics::Failure::refused(ix_build::wire::Code::RefusedMemoryBudget, class_name, "policy.memory_budget_bytes", budget.into(), bytes.into(), "requested memory reservation bytes")));
                }
            }
            let owner_queued = state
                .requests
                .iter()
                .filter(|request| request.owner == owner && request.phase == Phase::Queued)
                .count() as u64;
            if owner_queued >= u64::from(config.owner_queue_cap()) {
                return Err(Error::new(
                    ErrorCode::Request,
                    "owner queue full; change policy.max_queued_per_owner to raise the limit",
                )
                .with_failure(crate::diagnostics::Failure::refused(
                    ix_build::wire::Code::RefusedQueue,
                    class_name,
                    "policy.max_queued_per_owner",
                    config.owner_queue_cap().into(),
                    owner_queued.saturating_add(1).into(),
                    "queued requests for owner; change policy.max_queued_per_owner to raise limit",
                )));
            }
            if units > config.maximum_units {
                return Err(
                    Error::new(ErrorCode::Request, "units exceed configured maximum").with_failure(
                        crate::diagnostics::Failure::refused(
                            ix_build::wire::Code::RefusedQueue,
                            class_name,
                            "policy.maximum_units",
                            config.maximum_units.into(),
                            units.into(),
                            "requested units",
                        ),
                    ),
                );
            }
            if priority > config.max_priority() {
                return Err(
                    Error::new(ErrorCode::Request, "priority exceeds configured maximum")
                        .with_failure(crate::diagnostics::Failure::refused(
                            ix_build::wire::Code::InvalidRequest,
                            class_name,
                            "policy.priority_classes.maximum",
                            config.max_priority().into(),
                            priority.into(),
                            "requested priority",
                        )),
                );
            }
            if units == 0 {
                return Err(Error::new(
                    ErrorCode::Request,
                    "units or priority outside configured bounds",
                ));
            }
            if state.requests.len() >= crate::config::MAX_REQUESTS {
                return Err(Error::new(
                    ErrorCode::State,
                    "retained request limit reached; prune terminal requests",
                )
                .with_failure(crate::diagnostics::Failure::refused(
                    ix_build::wire::Code::RefusedQueue,
                    class_name,
                    "policy.MAX_REQUESTS",
                    (crate::config::MAX_REQUESTS as u64).into(),
                    (state.requests.len() as u64).into(),
                    "retained requests",
                )));
            }
            let (memory_cargo_jobs, memory_rust_test_threads, memory_parallelism) =
                resolve_build_parallelism(
                    config,
                    command,
                    caller_cargo_build_jobs.as_deref(),
                    caller_rust_test_threads.as_deref(),
                    units,
                    state.used().saturating_add(units),
                );
            let memory_cap_bytes = if memory.is_some() { memory } else {
                if config.memory_admission == crate::config::MemoryAdmissionMode::LeaseAccounted {
                    Some(estimate_memory_lease(
                        config,
                        class_name,
                        memory_parallelism,
                    )?)
                } else {
                    None
                }};
            let memory_reservation_bytes = if memory.is_some() { memory } else { memory_cap_bytes
                .map(|_| estimate_memory_reservation(config, class_name, memory_parallelism))
                .transpose()? };
            if memory.is_none() && let Some(reservation) = memory_reservation_bytes {
                validate_memory_reservation(config, class_name, memory_parallelism, reservation)?;
            }
            state.requests.push(Request {
                telemetry: telemetry.clone(),
                client: Some(Processes::self_identity()?),
                client_pid_namespace: crate::process::pid_namespace()?,
                supervision_detail: None,
                pid_namespace: crate::process::pid_namespace()?,
                trace: ix_build::wire::RequestTrace {
                    lifecycle_state: "queued".to_owned(),
                    queue_depth_at_enqueue: u32::try_from(
                        state
                            .requests
                            .iter()
                            .filter(|request| request.phase == Phase::Queued)
                            .count()
                            .saturating_add(1),
                    )
                    .unwrap_or(u32::MAX),
                    queued_at_ms: Some(now),
                    ..attempt_trace.clone()
                },
                command_started_clock_ms: None,
                blocked_at_clock_ms: None,
                active_blocked_reasons: Vec::new(),
                measured_memory_bytes: None,
                id: id.clone(),
                sequence,
                owner: owner.to_owned(),
                ticket: ticket.to_owned(),
                units,
                priority,
                class: class_name.to_owned(),
                memory_cap_bytes,
                memory_reservation_bytes,
                memory_limit_key: memory_cap_bytes.map(|_| {
                    if memory.is_some() { "request.memory_bytes".to_owned() } else if config.memory_per_job_by_class.contains_key(class_name) {
                        format!("policy.memory_per_job_by_class.{class_name}*parallelism")
                    } else {
                        "policy.memory_limit_per_job_bytes*parallelism".to_owned()
                    }
                }),
                memory_cargo_jobs,
                memory_rust_test_threads,
                memory_parallelism: Some(memory_parallelism),
                caller_cargo_build_jobs,
                caller_rust_test_threads,
                caller_cargo_incremental,
                caller_cargo_target_dir,
                lease_record: None,
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
            Ok(id.clone())
        })();
        if let Err(error) = result {
            let mut failure = crate::diagnostics::unscoped(&error, id.0.clone());
            failure.request_id = id.0.clone();
            failure.class = class_name.to_owned();
            if let Some(message) = &mut failure.diagnostic.message {
                *message = message.replace("request=pending ", &format!("request={} ", id.0));
            }
            state.events.push(Event {
                telemetry: telemetry.clone(),
                correction: false,
                trace: ix_build::wire::RequestTrace {
                    diagnostic: failure.diagnostic.clone(),
                    lifecycle_state: "refused".to_owned(),
                    finished_at_ms: Some(now),
                    ..attempt_trace
                },
                kind: "refused".to_owned(),
                schema_version: 1,
                event_id: format!("{}:e{}", state.store_id, state.next_event),
                request_id: id,
                host: state.host.clone(),
                owner: owner.to_owned(),
                ticket: ticket.to_owned(),
                observed_at_ms: now,
                state: Phase::Failed,
                units,
                priority,
                class: class_name.to_owned(),
                capacity_units: config.capacity_units,
                free_units: config.capacity_units.saturating_sub(state.used()),
                wait_ms: 0,
                hold_ms: None,
                reason: Some("refused".to_owned()),
            });
            state.next_event = state
                .next_event
                .checked_add(1)
                .ok_or_else(|| Error::new(ErrorCode::State, "event sequence exhausted"))?;
            return Ok(Err(error.with_failure(failure)));
        }
        Ok(result)
    })?
}

#[allow(clippy::too_many_arguments)]
fn requested_trace(
    config: &Config,
    class: &str,
    command: &[String],
    jobs_env: Option<&str>,
    threads_env: Option<&str>,
    units: u32,
    active_units: u32,
) -> ix_build::wire::RequestTrace {
    let (jobs, threads, _) =
        resolve_build_parallelism(config, command, jobs_env, threads_env, units, active_units);
    let mut args = command.get(1..).unwrap_or_default().to_vec();
    let requested_jobs = clamp_argument(&mut args, &["--jobs", "-j"], false, None)
        .and_then(|argument| argument.value)
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| jobs_env.and_then(|value| value.parse().ok()));
    let requested_threads = clamp_test_threads(&mut args, None)
        .and_then(|argument| argument.value)
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| threads_env.and_then(|value| value.parse().ok()));
    let parallelism = u64::from(
        requested_jobs
            .unwrap_or(jobs.unwrap_or(units))
            .max(requested_threads.unwrap_or(threads.unwrap_or(0))),
    );
    let accounted = config.memory_admission == crate::config::MemoryAdmissionMode::LeaseAccounted;
    ix_build::wire::RequestTrace {
        requested_jobs,
        requested_memory_bytes: accounted
            .then(|| {
                parallelism.checked_mul(
                    config
                        .memory_per_job_by_class
                        .get(class)
                        .copied()
                        .unwrap_or(config.memory_limit_per_job_bytes),
                )
            })
            .flatten(),
        requested_memory_reservation_bytes: accounted
            .then(|| {
                parallelism.checked_mul(
                    config
                        .memory_reserve_per_job_by_class
                        .get(class)
                        .copied()
                        .unwrap_or(config.memory_reserve_per_job_bytes),
                )
            })
            .flatten(),
        ..Default::default()
    }
}

fn estimate_memory_lease(
    config: &crate::config::Config,
    class: &str,
    parallelism: u32,
) -> Result<u64> {
    let per_job = config
        .memory_per_job_by_class
        .get(class)
        .copied()
        .unwrap_or(config.memory_limit_per_job_bytes);
    let lease_bytes = u64::from(parallelism).checked_mul(per_job).ok_or_else(|| {
        Error::new(
            ErrorCode::Request,
            "requested memory lease size overflows the supported range",
        )
    })?;
    if config
        .memory_cap_by_class
        .get(class)
        .is_some_and(|cap| lease_bytes > *cap)
    {
        return Err(Error::new(
            ErrorCode::Request,
            format!("requested memory lease {lease_bytes} exceeds class `{class}` memory cap"),
        )
        .with_failure(crate::diagnostics::Failure::refused(
            ix_build::wire::Code::RefusedClassCap,
            class,
            &format!("policy.memory_cap_by_class.{class}"),
            config
                .memory_cap_by_class
                .get(class)
                .copied()
                .unwrap_or(0)
                .into(),
            lease_bytes.into(),
            "requested memory bytes",
        )));
    }
    estimate_memory_reservation(config, class, parallelism)?;
    Ok(lease_bytes)
}

/// Provenance: SWM-437. Trace: FR-005-AC-4, FR-015-AC-8
fn estimate_memory_reservation(config: &Config, class: &str, parallelism: u32) -> Result<u64> {
    let per_job = config
        .memory_reserve_per_job_by_class
        .get(class)
        .copied()
        .unwrap_or(config.memory_reserve_per_job_bytes);
    let lease_bytes = u64::from(parallelism)
        .checked_mul(per_job)
        .ok_or_else(|| Error::new(ErrorCode::Request, "requested memory reservation overflows"))?;
    Ok(lease_bytes)
}

/// Provenance: SWM-437. Trace: FR-005-AC-4, FR-015-AC-8
fn validate_memory_reservation(
    config: &Config,
    class: &str,
    parallelism: u32,
    lease_bytes: u64,
) -> Result<()> {
    let per_job = config
        .memory_reserve_per_job_by_class
        .get(class)
        .copied()
        .unwrap_or(config.memory_reserve_per_job_bytes);
    let budget = config.memory_budget_bytes.unwrap_or(0);
    if lease_bytes > budget {
        return Err(Error::new(
            ErrorCode::Request,
            format!("memory reservation {lease_bytes} exceeds memory budget {budget}; per-job reservation {per_job}; effective jobs {parallelism}"),
        )
        .with_failure(crate::diagnostics::Failure::refused(
            ix_build::wire::Code::RefusedMemoryBudget,
            class,
            "policy.memory_budget_bytes",
            budget.into(),
            lease_bytes.into(),
            &format!("memory reservation bytes; per_job_bytes={per_job}; effective_jobs={parallelism}"),
        )));
    }
    Ok(())
}

/// Provenance: SWM-437. Trace: FR-005-AC-4, FR-015-AC-8
fn resolve_build_parallelism(
    config: &crate::config::Config,
    command: &[String],
    caller_cargo_build_jobs: Option<&str>,
    caller_rust_test_threads: Option<&str>,
    units: u32,
    active_units: u32,
) -> (Option<u32>, Option<u32>, u32) {
    let is_cargo = command
        .first()
        .and_then(|program| std::path::Path::new(program).file_name())
        .is_some_and(|name| name == "cargo");
    let mut bounded_config = config.clone();
    let mut arguments = if is_cargo {
        command.get(1..).unwrap_or_default().to_vec()
    } else {
        bounded_config.cargo_build_jobs = Some(units);
        bounded_config.rust_test_threads = Some(units);
        Vec::new()
    };
    let prepared = prepare_build_environment(
        &bounded_config,
        &mut arguments,
        caller_cargo_build_jobs,
        caller_rust_test_threads,
        if is_cargo { active_units } else { 1 },
    );
    if is_cargo {
        let jobs = prepared.record.cargo_build_jobs.unwrap_or(units);
        let test_threads = prepared.record.rust_test_threads;
        (Some(jobs), test_threads, jobs.max(1))
    } else {
        (None, prepared.record.rust_test_threads, units)
    }
}

/// Reservation evaluated with the jobs that would be granted now, not at enqueue.
/// Provenance: SWM-437. Trace: FR-005-AC-4, FR-015-AC-8
pub(crate) fn admission_reservation(
    request: &Request,
    config: &Config,
    active_units: u32,
) -> Result<u64> {
    if request.memory_parallelism.is_none()
        || request.memory_limit_key.as_deref() == Some("request.memory_bytes")
        || config.memory_admission != crate::config::MemoryAdmissionMode::LeaseAccounted
    {
        return Ok(request.reservation_bytes());
    }
    let (_, _, jobs) = resolve_build_parallelism(
        config,
        &request.command,
        request.caller_cargo_build_jobs.as_deref(),
        request.caller_rust_test_threads.as_deref(),
        request.units,
        active_units,
    );
    estimate_memory_reservation(config, &request.class, jobs)
}

fn validate_parallelism_input(
    command: &[String],
    cargo_jobs_environment: Option<&str>,
    test_threads_environment: Option<&str>,
) -> Result<()> {
    let is_cargo = command
        .first()
        .and_then(|program| std::path::Path::new(program).file_name())
        .is_some_and(|name| name == "cargo");
    let invalid_jobs = || {
        Error::new(
            ErrorCode::Request,
            "Cargo job limits must be positive integers",
        )
    };
    let invalid_threads = || {
        Error::new(
            ErrorCode::Request,
            "Rust test thread limits must be positive integers",
        )
    };
    if let Some(value) = cargo_jobs_environment
        && is_cargo
        && !value.parse::<u32>().is_ok_and(|jobs| jobs > 0)
    {
        return Err(invalid_jobs());
    }
    if let Some(value) = test_threads_environment
        && !value.parse::<u32>().is_ok_and(|threads| threads > 0)
    {
        return Err(invalid_threads());
    }
    if !is_cargo {
        return Ok(());
    }
    let arguments = command.get(1..).unwrap_or_default();
    let end = arguments
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(arguments.len());
    let mut index = 0;
    while index < end {
        let argument = &arguments[index];
        if argument == "-j" || argument == "--jobs" {
            let value = arguments.get(index + 1).filter(|_| index + 1 < end);
            if !value.is_some_and(|value| value.parse::<u32>().is_ok_and(|jobs| jobs > 0)) {
                return Err(invalid_jobs());
            }
            index += 2;
            continue;
        }
        let long_value = argument
            .strip_prefix("--jobs=")
            .or_else(|| argument.strip_prefix("-j="))
            .or_else(|| {
                argument
                    .strip_prefix("-j")
                    .filter(|value| !value.is_empty())
            });
        if let Some(value) = long_value
            && !value.parse::<u32>().is_ok_and(|jobs| jobs > 0)
        {
            return Err(invalid_jobs());
        }
        index += 1;
    }
    let separator = arguments.iter().position(|argument| argument == "--");
    if let Some(separator) = separator {
        let after = &arguments[separator + 1..];
        let mut index = 0;
        while index < after.len() {
            if after[index] == "--test-threads" {
                if !after
                    .get(index + 1)
                    .is_some_and(|value| value.parse::<u32>().is_ok_and(|threads| threads > 0))
                {
                    return Err(invalid_threads());
                }
                index += 2;
                continue;
            }
            if let Some(value) = after[index].strip_prefix("--test-threads=")
                && !value.parse::<u32>().is_ok_and(|threads| threads > 0)
            {
                return Err(invalid_threads());
            }
            index += 1;
        }
    }
    Ok(())
}

fn prepare_request_environment(
    config: &crate::config::Config,
    request: &Request,
    arguments: &mut [String],
    active_units: u32,
) -> PreparedBuildEnvironment {
    if let Some(parallelism) = request.memory_parallelism {
        let is_cargo = request
            .command
            .first()
            .and_then(|program| std::path::Path::new(program).file_name())
            .is_some_and(|name| name == "cargo");
        let mut reserved_config = config.clone();
        reserved_config.cargo_build_jobs = Some(request.memory_cargo_jobs.unwrap_or(parallelism));
        reserved_config.rust_test_threads = if is_cargo {
            request.memory_rust_test_threads
        } else {
            Some(parallelism)
        };
        let mut no_arguments = Vec::new();
        let environment_arguments = if is_cargo {
            arguments
        } else {
            no_arguments.as_mut_slice()
        };
        prepare_build_environment(
            &reserved_config,
            environment_arguments,
            request.caller_cargo_build_jobs.as_deref(),
            request.caller_rust_test_threads.as_deref(),
            1,
        )
    } else {
        prepare_build_environment(
            config,
            arguments,
            request.caller_cargo_build_jobs.as_deref(),
            request.caller_rust_test_threads.as_deref(),
            active_units,
        )
    }
}

pub(crate) fn priority_for_class(store: &Store, class: &str) -> Result<u32> {
    store.transaction(|_, config| {
        config
            .priority_classes
            .get(class)
            .copied()
            .or_else(|| (class == "periodic").then_some(0))
            .ok_or_else(|| Error::new(ErrorCode::Request, "priority class is not configured"))
    })
}

pub(crate) fn spawn_worker(store: &Store, id: &RequestId) -> Result<Option<Child>> {
    spawn_worker_with(store, id, false, |claim, lease| {
        launch_worker(store, claim, lease)
    })
}

pub(crate) fn handoff_worker(store: &Store, id: &RequestId) -> Result<Option<Child>> {
    if !cfg!(target_os = "linux") {
        return Err(Error::new(
            ErrorCode::Request,
            "live queued-worker handoff requires Linux; drain legacy callers on other hosts",
        ));
    }
    spawn_worker_with(store, id, true, |claim, lease| {
        launch_worker(store, claim, lease)
    })
}

fn spawn_worker_with<T>(
    store: &Store,
    id: &RequestId,
    handoff: bool,
    spawn: impl FnOnce(&crate::model::WorkerClaim, std::fs::File) -> Result<T>,
) -> Result<Option<T>> {
    let Some(_admission) = store.try_admission_lock()? else {
        return Ok(None);
    };
    let Some(lease) = store.try_lock_member(&format!("{}.spawn.lock", id.0))? else {
        return Ok(None);
    };
    let launcher = Processes::self_identity()?;
    let pid_namespace = crate::process::pid_namespace()?;
    let claim = store.transaction(|state, _| {
        let store_id = state.store_id.clone();
        let request = state.find_mut(id)?;
        if request.phase != Phase::Queued {
            return Ok(None);
        }
        let previous = store.claim(id)?;
        let claim_namespace = previous
            .as_ref()
            .filter(|claim| {
                claim.store_id == store_id && claim.generation == request.worker_generation
            })
            .map(|claim| &claim.pid_namespace)
            .unwrap_or(&request.pid_namespace);
        let origin_namespace = if request.pid_namespace.is_some() {
            &request.pid_namespace
        } else {
            claim_namespace
        };
        if origin_namespace != &pid_namespace {
            return Ok(None);
        }
        let pending = previous.as_ref().is_some_and(|claim| {
            matches!(claim.protocol, 1 | 2)
                && claim.store_id == store_id
                && claim.request_id == *id
                && claim.generation == request.worker_generation
                && (claim.worker.is_none() || claim.worker.as_ref() != request.worker.as_ref())
        });
        if !pending
            && request
                .worker
                .as_ref()
                .is_some_and(|worker| !group_absent(worker.pid))
            && !handoff
        {
            return Ok(None);
        }
        request.worker_generation = request
            .worker_generation
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "supervisor generation exhausted"))?;
        // Until the child registers, the launcher owns the durable claim.
        // Unknown namespace + a nonempty owner also prevents older Linux
        // recovery loops from spawning into this pre-fork interval.
        request.worker = Some(launcher.clone());
        request.pid_namespace = None;
        let claim = crate::model::WorkerClaim {
            protocol: 2,
            store_id,
            request_id: id.clone(),
            generation: request.worker_generation,
            launcher: launcher.clone(),
            pid_namespace: pid_namespace.clone(),
            worker: None,
        };
        store.write_claim(&claim)?;
        Ok(Some(claim))
    })?;
    let Some(claim) = claim else {
        return Ok(None);
    };
    spawn(&claim, lease).map(Some)
}

fn launch_worker(
    store: &Store,
    claim: &crate::model::WorkerClaim,
    lease: std::fs::File,
) -> Result<Child> {
    let id = &claim.request_id;
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
    let child = Command::new(store.service_binary()?)
        .arg("--state")
        .arg(&store.path)
        .arg("--config")
        .arg(&store.config_path)
        .arg("worker")
        .arg(&id.0)
        .arg(claim.generation.to_string())
        .stdin(Stdio::from(lease))
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()?;
    Ok(child)
}

/// Redacted snapshot suitable for deterministic monitoring consumers.
#[derive(Serialize)]
pub struct Report {
    /// Sum of reservations held by active leases.
    pub memory_reserved_bytes: u64,
    /// Whole host admission budget for comparison with active and head reservations.
    pub memory_budget_bytes: Option<u64>,
    /// Durable admission hold; queued identity and ordering remain unchanged.
    pub queue_hold: Option<crate::queue::Hold>,
    /// Effective policy bound for explicitly sized memory requests.
    pub max_request_memory: Option<u64>,
    /// Effective queued request limit for each owner.
    pub max_queued_per_owner: u32,
    /// Queued request counts grouped by owner; running and terminal work excluded.
    pub queued_by_owner: std::collections::BTreeMap<String, u32>,
    /// Durable counters and current queue timing metrics.
    pub metrics: ix_build::wire::MetricsSnapshot,
    /// Effective shared admission state directory.
    pub state_path: std::path::PathBuf,
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
    /// Shared diagnostic and queue analysis fields.
    #[serde(flatten)]
    pub trace: ix_build::wire::RequestTrace,
    /// Memory needed by this queued request.
    pub memory_needed_bytes: Option<u64>,
    /// Currently unreserved host memory budget.
    pub memory_free_budget_bytes: Option<u64>,
    /// Namespace in which stored process IDs must be interpreted.
    pub pid_namespace: Option<String>,
    /// Stable request identity.
    pub request_id: RequestId,
    /// Admitted cap in bytes, preserved even if policy changes after admission.
    pub memory_cap_bytes: Option<u64>,
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
    /// Named priority class selected for the request.
    pub class: String,
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
    /// Effective settings and memory enforcement selected for this lease.
    pub lease_record: Option<LeaseRecord>,
}

pub(crate) fn recover(store: &Store) -> Result<Vec<RequestId>> {
    recover_with_processes(store, Processes::read)
}

fn recover_with_processes(
    store: &Store,
    read_processes: impl FnOnce() -> Processes,
) -> Result<Vec<RequestId>> {
    recover_with_boot(
        store,
        read_processes,
        crate::process::boot_id(),
        sysinfo::System::boot_time(),
    )
}

fn recover_with_boot(
    store: &Store,
    read_processes: impl FnOnce() -> Processes,
    boot_id: Option<String>,
    boot_time: u64,
) -> Result<Vec<RequestId>> {
    store.transaction(|state, config| {
        let now = now_ms()?;
        let clock = clock_ms()?;
        let processes = read_processes();
        let pid_namespace = crate::process::pid_namespace()?;
        let installed_host = crate::writable::is_installed_host_namespace(&store.path, boot_id.as_deref(), pid_namespace.as_deref());
        let namespaces = installed_host.then(|| crate::process::namespaces_at(std::path::Path::new("/proc")).ok()).flatten();
        // sysinfo's macOS backend reads kern.boottime. Other timestamp-only
        // backends get the same 120-second tolerance; Linux uses the UUID.
        let rebooted = state.rebooted(boot_id.as_deref(), boot_time);
        if rebooted {
            state.system_event("boot_changed", config, now)?;
            let new_boot_ms = boot_time.saturating_mul(1_000);
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
            let mut origin_namespace = request.pid_namespace.clone();
            if request.phase == Phase::Queued && let Some(claim) = store.claim(&request.id)?
                && matches!(claim.protocol, 1 | 2) && claim.store_id == state.store_id && claim.generation == request.worker_generation
                && (claim.worker.is_none() || claim.worker.as_ref() != request.worker.as_ref()) {
                if claim.pid_namespace == pid_namespace {
                    respawn.push(request.id.clone());
                    continue;
                }
                origin_namespace = claim.pid_namespace;
            }
            if !rebooted && origin_namespace != pid_namespace {
                if origin_namespace.is_none() && request.phase == Phase::Queued
                    && request.worker.is_none() && request.watchdog.is_none() {
                    request.pid_namespace = pid_namespace.clone();
                } else if origin_namespace.as_ref().is_some_and(|namespace|
                    namespaces.as_ref().and_then(|snapshot| snapshot.alive(namespace)) == Some(false)) {
                    // An extinct PID namespace cannot retain live children.
                    request.worker = None; request.watchdog = None;
                    if request.phase == Phase::Running {
                        request.supervision_detail = Some("owning PID namespace is extinct; host procfs verified no namespace init remains".to_owned());
                        finish.push(request.id.clone());
                    } else { request.pid_namespace = pid_namespace.clone(); respawn.push(request.id.clone()); }
                    continue;
                } else {
                // Namespace-local PIDs can refer to unrelated host processes.
                // Only the owning namespace may infer liveness or respawn.
                request.reason = Some("foreign_pid_namespace_live_or_unverifiable".to_owned());
                continue;
                }
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
                    ) && request.worker.as_ref().is_none_or(|worker| group_absent(worker.pid)) {
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
            state.system_event("recovered", config, now)?;
        }
        state.boot_time = boot_time;
        state.boot_id = boot_id;
        Ok(respawn)
    })
}

pub(crate) fn status(store: &Store) -> Result<Report> {
    status_with_clock(store, || Ok((now_ms()?, clock_ms()?)))
}

#[cfg(test)]
fn status_at(store: &Store, now: u64, clock: u64) -> Result<Report> {
    status_with_clock(store, || Ok((now, clock)))
}

/// Provenance: SWM-437. Trace: FR-005-AC-4, FR-015-AC-8
fn status_with_clock(
    store: &Store,
    mut read_clock: impl FnMut() -> Result<(u64, u64)>,
) -> Result<Report> {
    let (state, config) = store.snapshot()?;
    let (now, clock) = read_clock()?;
    let processes = state
        .requests
        .iter()
        .any(|request| request.phase == Phase::Running && request.worker.is_some())
        .then(|| {
            Processes::read_workers(
                &state
                    .requests
                    .iter()
                    .filter(|request| request.phase == Phase::Running)
                    .filter_map(|request| request.worker.as_ref().map(|worker| worker.pid))
                    .collect::<Vec<_>>(),
            )
        });
    let sample = config.sample(now);
    let (sample, source_error) = match sample {
        Ok(sample) => (Some(sample), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let positions = state.queue_positions(&config, clock);
    let used = state.used();
    let free_budget = config
        .memory_budget_bytes
        .map(|budget| budget.saturating_sub(state.memory_used(&config)));
    let requests = state
        .requests
        .iter()
        .map(|request| {
            let ended_clock = request.ended_clock_ms.unwrap_or(clock);
            let identity = request.worker.as_ref().and_then(|worker| {
                processes
                    .as_ref()
                    .map(|processes| processes.identity(worker, &request.id))
            });
            Ok(RequestReport {
                trace: {
                    let mut trace = request.trace_snapshot(clock);
                    if request.phase == Phase::Queued {
                        trace.position = positions.get(&request.id.0).copied();
                    }
                    trace
                },
                memory_needed_bytes: if request.phase == Phase::Queued
                    && config.memory_admission == crate::config::MemoryAdmissionMode::LeaseAccounted
                {
                    admission_reservation(request, &config, used.saturating_add(request.units))
                        .ok()
                        .or(request.memory_reservation_bytes)
                        .or(request.memory_cap_bytes)
                } else {
                    request
                        .memory_reservation_bytes
                        .or(request.memory_cap_bytes)
                },
                memory_free_budget_bytes: free_budget,
                pid_namespace: request.pid_namespace.clone(),
                request_id: request.id.clone(),
                memory_cap_bytes: request.memory_cap_bytes,
                owner: request.owner.clone(),
                ticket: request.ticket.clone(),
                state: phase_name(request.phase).to_owned(),
                units: request.units,
                priority: request.priority,
                class: request.class.clone(),
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
                            now.saturating_sub(heartbeat) > crate::config::HEARTBEAT_MAX_AGE_MS
                        })),
                reason: request.reason.clone(),
                exit_code: request.exit_code,
                signal: request.signal,
                lease_record: request.lease_record.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Report {
        memory_reserved_bytes: state.memory_used(&config),
        memory_budget_bytes: config.memory_budget_bytes,
        queue_hold: store.queue_hold()?,
        max_request_memory: config.request_memory_cap(),
        max_queued_per_owner: config.owner_queue_cap(),
        queued_by_owner: state
            .requests
            .iter()
            .filter(|request| request.phase == Phase::Queued)
            .fold(std::collections::BTreeMap::new(), |mut counts, request| {
                *counts.entry(request.owner.clone()).or_insert(0) += 1;
                counts
            }),
        metrics: state
            .metrics
            .snapshot_with_policy(&state, &config, now, clock)?,
        state_path: store.path.clone(),
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
}

/// Collect using the deadline established before opening the metrics store.
pub(crate) fn metrics_before(
    store: &Store,
    sampling_deadline: std::time::Instant,
) -> Result<String> {
    metrics_with_clock(store, || Ok((now_ms()?, clock_ms()?)), sampling_deadline)
}

#[cfg(test)]
pub(crate) fn metrics(store: &Store) -> Result<String> {
    metrics_before(
        store,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
}

#[cfg(test)]
fn metrics_at(store: &Store, now: u64, clock: u64) -> Result<String> {
    metrics_with_clock(
        store,
        || Ok((now, clock)),
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
}

fn metrics_with_clock(
    store: &Store,
    mut read_clock: impl FnMut() -> Result<(u64, u64)>,
    sampling_deadline: std::time::Instant,
) -> Result<String> {
    let (state, config) = store.snapshot()?;
    let (now, clock) = read_clock()?;
    let mut projection = state.metrics.clone();
    let queued_workers: Vec<_> = state
        .requests
        .iter()
        .filter(|r| r.phase == Phase::Queued)
        .filter_map(|r| r.worker.as_ref().map(|w| w.pid))
        .collect();
    let mut processes = None;
    projection.expire_stale_block(
        &state.requests,
        now,
        clock,
        crate::config::POLL_INTERVAL_MS,
        |request| {
            request.worker.as_ref().is_some_and(|worker| {
                matches!(
                    processes
                        .get_or_insert_with(|| Processes::read_workers(&queued_workers))
                        .identity(worker, &request.id),
                    IdentityState::Matching | IdentityState::Unavailable
                )
            })
        },
    )?;
    let mut text = projection.exposition(&state, &config, now, clock)?;
    let host = crate::metrics::escape_label(&state.host);
    if let Some((missing, unleased)) = crate::sample::lease_health(&state) {
        crate::telemetry::append_gauge(
            &mut text,
            "ix_build_leases_missing_process",
            &host,
            missing as f64,
        );
        crate::telemetry::append_gauge(
            &mut text,
            "ix_build_unleased_build_processes",
            &host,
            unleased as f64,
        );
    }
    let disk = nix::sys::statvfs::statvfs(&store.path)
        .map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
    crate::telemetry::append_gauge(
        &mut text,
        "ix_build_state_dir_free_bytes",
        &host,
        disk.blocks_available() as f64 * disk.fragment_size() as f64,
    );
    let size = std::fs::metadata(store.path.join("snapshot.json")).map_or(0, |m| m.len());
    crate::telemetry::append_gauge(
        &mut text,
        "ix_build_snapshot_size_bytes",
        &host,
        size as f64,
    );
    crate::sample::kache::append(
        &mut text,
        &state.host,
        &store.path.join("kache-metrics.json"),
        sampling_deadline,
        &config.cache,
    );
    crate::queue::append_metrics(&mut text, &state.host, store.queue_hold()?.as_ref(), now);
    let checkpoint = crate::model::ExportCheckpoint {
        store_id: state.store_id.clone(),
        sequence: state.metrics.exported_sequence(),
    };
    text.push_str(&format!(
        "# ix-build-checkpoint {}\n",
        serde_json::to_string(&checkpoint)?
    ));
    Ok(text)
}

#[cfg(test)]
pub(crate) fn metrics_to_file(store: &Store, path: &Path) -> Result<()> {
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
    cancel_with_clock(store, id, || Ok((now_ms()?, clock_ms()?)))
}

#[cfg(test)]
fn cancel_at(store: &Store, id: &RequestId, now: u64, clock: u64) -> Result<()> {
    cancel_with_clock(store, id, || Ok((now, clock)))
}

fn cancel_with_clock(
    store: &Store,
    id: &RequestId,
    read_clock: impl FnMut() -> Result<(u64, u64)>,
) -> Result<()> {
    store.transaction_with_clock(read_clock, |state, config, now, clock| {
        let request = state.find_mut(id)?;
        if request.phase.terminal() {
            return Ok(());
        }
        request.stop = Some(StopReason::Cancelled);
        if request.phase == Phase::Queued {
            state.finish(id, Completion::default(), config, now, clock)?;
        }
        Ok(())
    })
}

pub(crate) fn wait(
    store: &Store,
    id: &RequestId,
    mut workers: Vec<Child>,
) -> Result<(i32, Option<String>)> {
    let mut last_snapshot = None;
    let mut observed = None;
    loop {
        let mut active = Vec::new();
        for mut worker in workers {
            if worker.try_wait()?.is_none() {
                active.push(worker);
            }
        }
        workers = active;
        let notice = store.notice(id).ok().flatten();
        let refresh = last_snapshot
            .is_none_or(|last: std::time::Instant| last.elapsed() >= Duration::from_secs(2))
            || notice.as_ref().is_none_or(|notice| notice.phase.terminal())
            || observed.as_ref().is_none_or(|request: &Request| {
                notice.as_ref().is_some_and(|notice| {
                    notice.phase != request.phase || notice.generation != request.worker_generation
                })
            });
        if refresh {
            let (state, _) = store.snapshot()?;
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
            if let Some(code) = code {
                return Ok((code, request.reason.clone()));
            }
            observed = Some(request.clone());
            last_snapshot = Some(std::time::Instant::now());
        }
        if observed
            .as_ref()
            .is_some_and(|request| request.phase == Phase::Queued)
        {
            if let Some(child) = spawn_worker(store, id)? {
                workers.push(child);
            }
            crate::scheduler::ensure(store)?;
        } else if let Some(request) = observed.as_ref() {
            let missing = if store.claim(id)?.is_some() {
                store
                    .try_lock_member(&format!("{}.spawn.lock", id.0))?
                    .is_some()
            } else {
                request.pid_namespace == crate::process::pid_namespace()?
                    && request
                        .worker
                        .as_ref()
                        .is_some_and(|worker| group_absent(worker.pid))
            };
            if missing {
                crate::scheduler::ensure(store)?;
            }
        }
        std::thread::sleep(Duration::from_millis(wait_interval()));
    }
}

fn wait_interval() -> u64 {
    let half = crate::config::POLL_INTERVAL_MS / 2;
    half + u64::from(std::process::id()) % half.max(1)
}

#[derive(Clone)]
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
    store.transaction(|state, config| {
        let request = state.find_mut(id)?;
        if request.phase != Phase::Queued || request.worker.is_some() {
            return Ok(None);
        }
        request.worker_generation = request
            .worker_generation
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "supervisor generation exhausted"))?;
        request.worker = Some(identity.clone());
        let fence = WorkerFence {
            identity: identity.clone(),
            generation: request.worker_generation,
        };
        if fence.generation > 1 {
            state.system_event("supervisor_restart", config, now_ms()?)?;
            state.system_event("recovered", config, now_ms()?)?;
        }
        Ok(Some(fence))
    })
}

fn claim_spawned_worker(
    store: &Store,
    id: &RequestId,
    identity: &crate::model::ProcessIdentity,
    generation: u64,
) -> Result<Option<WorkerFence>> {
    let namespace = crate::process::pid_namespace()?;
    store.transaction(|state, config| {
        let Some(mut claim) = store.claim(id)? else {
            return Ok(None);
        };
        if claim.store_id != state.store_id {
            return Ok(None);
        }
        let request = state.find_mut(id)?;
        if !matches!(claim.protocol, 1 | 2)
            || claim.pid_namespace != namespace
            || claim.request_id != *id
            || claim.generation != generation
            || request.phase != Phase::Queued
            || request.worker_generation != generation
            || claim.worker.is_some()
        {
            return Ok(None);
        }
        request.worker = Some(identity.clone());
        request.pid_namespace = namespace;
        request.heartbeat_at_ms = Some(now_ms()?);
        claim.worker = Some(identity.clone());
        store.write_claim(&claim)?;
        if generation > 1 {
            state.system_event("supervisor_restart", config, now_ms()?)?;
            state.system_event("recovered", config, now_ms()?)?;
        }
        Ok(Some(WorkerFence {
            identity: identity.clone(),
            generation,
        }))
    })
}

pub(crate) fn schedule_queue(store: &Store) -> Result<()> {
    // Keep the shared guard through the grant transaction: a drain hold cannot
    // start between checking admission and publishing a running lease.
    // Provenance: SWM-429. Trace: FR-005-AC-4, FR-011-AC-7
    let admission = store.try_admission_lock()?;
    let (snapshot, _) = store.snapshot()?;
    let claims: std::collections::BTreeMap<_, _> = snapshot
        .requests
        .iter()
        .filter(|request| request.phase == Phase::Queued)
        .filter_map(|request| {
            store
                .claim(&request.id)
                .ok()
                .flatten()
                .filter(|_| {
                    matches!(
                        store.try_lock_member(&format!("{}.spawn.lock", request.id.0)),
                        Ok(None)
                    )
                })
                .map(|claim| (request.id.0.clone(), claim))
        })
        .collect();
    store.transaction_with_clock(
        || Ok((now_ms()?, clock_ms()?)),
        |state, config, now, clock| {
            let sample = config.sample(now).ok();
            while admission.is_some() {
                if refuse_impossible_head(state, config, now, clock)? {
                    continue;
                }
                let positions = state.queue_positions(config, clock);
                let mut ordered: Vec<_> = positions.iter().collect();
                ordered.sort_by_key(|(_, position)| **position);
                let mut candidate = None;
                for (id, _) in ordered {
                    let id = RequestId(id.clone());
                    let request = state.find(&id)?;
                    let ready = claims.get(&id.0).is_some_and(|claim| {
                        matches!(claim.protocol, 1 | 2)
                            && claim.store_id == state.store_id
                            && claim.request_id == id
                            && claim.generation == request.worker_generation
                            && claim.worker.is_some()
                            && claim.worker.as_ref() == request.worker.as_ref()
                    });
                    if ready
                        && request.stop.is_none()
                        && state
                            .admission_reason(&id, config, sample.as_ref(), now, clock)?
                            .is_none()
                    {
                        candidate = Some(id);
                        break;
                    }
                }
                let Some(id) = candidate else {
                    break;
                };
                if refuse_impossible_request(state, config, &id, now, clock)? {
                    continue;
                }
                grant_request(state, config, &id, now, clock)?;
            }
            let positions = state.queue_positions(config, clock);
            let queued: Vec<_> = state
                .requests
                .iter()
                .filter(|request| request.phase == Phase::Queued)
                .map(|request| request.id.clone())
                .collect();
            for id in queued {
                let registered = claims.get(&id.0).is_some_and(|claim| {
                    state.find(&id).is_ok_and(|request| {
                        matches!(claim.protocol, 1 | 2)
                            && claim.store_id == state.store_id
                            && claim.generation == request.worker_generation
                            && claim.worker.is_some()
                            && claim.worker.as_ref() == request.worker.as_ref()
                    })
                });
                let reason = if admission.is_none() {
                    Some("install_drain")
                } else {
                    state.admission_reason(&id, config, sample.as_ref(), now, clock)?
                };
                let blocks = state.block_observations(&id, config, sample.as_ref(), now, clock)?;
                let request = state.find_mut(&id)?;
                // The worker-only lifetime lock is its queued liveness signal.
                // Keep telemetry fresh without asking every worker to write.
                if registered
                    && request
                        .heartbeat_at_ms
                        .is_none_or(|last| now.saturating_sub(last) >= 1_000)
                {
                    request.heartbeat_at_ms = Some(now);
                }
                let changed = request.observe_blocks(blocks, clock);
                request.trace.position = positions.get(&id.0).copied();
                request.reason = reason.map(str::to_owned);
                if request.worker.is_some() && !claims.contains_key(&id.0) {
                    request.reason = Some("legacy_worker_requires_reattach".to_owned());
                }
                if changed {
                    state.event(&id, config, now, clock)?;
                    if let Some(event) = state.events.last_mut() {
                        event.kind = "blocked".to_owned();
                    }
                }
            }
            let blocked = current_block_reason(state, config, sample.as_ref(), now, clock)?;
            state.metrics.observe_block(blocked, now, clock)?;
            Ok(())
        },
    )
}

/// Provenance: SWM-437. Trace: FR-005-AC-4, FR-015-AC-8
fn refuse_impossible_head(
    state: &mut crate::model::State,
    config: &Config,
    now: u64,
    clock: u64,
) -> Result<bool> {
    let Some(id) = state.next(config, clock).map(|request| request.id.clone()) else {
        return Ok(false);
    };
    refuse_impossible_request(state, config, &id, now, clock)
}

/// Provenance: SWM-437. Trace: FR-005-AC-4, FR-015-AC-8
fn refuse_impossible_request(
    state: &mut crate::model::State,
    config: &Config,
    id: &RequestId,
    now: u64,
    clock: u64,
) -> Result<bool> {
    let request = state.find(id)?;
    if config.memory_admission != crate::config::MemoryAdmissionMode::LeaseAccounted
        || request.memory_parallelism.is_none()
        || request.memory_limit_key.as_deref() == Some("request.memory_bytes")
    {
        return Ok(false);
    }
    let (_, _, jobs) = resolve_build_parallelism(
        config,
        &request.command,
        request.caller_cargo_build_jobs.as_deref(),
        request.caller_rust_test_threads.as_deref(),
        request.units,
        state.used().saturating_add(request.units),
    );
    let allocation = estimate_memory_reservation(config, &request.class, jobs)
        .and_then(|bytes| validate_memory_reservation(config, &request.class, jobs, bytes))
        .and_then(|()| estimate_memory_lease(config, &request.class, jobs));
    let Err(error) = allocation else {
        return Ok(false);
    };
    let id = request.id.clone();
    let mut failure = crate::diagnostics::unscoped(&error, id.0.clone());
    if let Some(message) = &mut failure.diagnostic.message {
        *message = message.replace("request=pending ", &format!("request={} ", id.0));
    }
    state.finish(
        &id,
        Completion {
            reason: Some("refused"),
            ..Completion::default()
        },
        config,
        now,
        clock,
    )?;
    let request = state.find_mut(&id)?;
    request.trace.lifecycle_state = "refused".into();
    request.trace.diagnostic = failure.diagnostic;
    let trace = request.trace_snapshot(clock);
    if let Some(event) = state.events.last_mut() {
        event.kind = "refused".into();
        event.trace = trace;
    }
    Ok(true)
}

/// Provenance: SWM-437. Trace: FR-005-AC-4, FR-015-AC-8
fn grant_request(
    state: &mut crate::model::State,
    config: &Config,
    id: &RequestId,
    now: u64,
    clock: u64,
) -> Result<()> {
    let active_units = state.used().saturating_add(state.find(id)?.units);
    let request = state.find_mut(id)?;
    if request.memory_parallelism.is_some()
        && config.memory_admission == crate::config::MemoryAdmissionMode::LeaseAccounted
    {
        let (jobs, threads, parallelism) = resolve_build_parallelism(
            config,
            &request.command,
            request.caller_cargo_build_jobs.as_deref(),
            request.caller_rust_test_threads.as_deref(),
            request.units,
            active_units,
        );
        if request.memory_limit_key.as_deref() != Some("request.memory_bytes") {
            request.memory_cap_bytes =
                Some(estimate_memory_lease(config, &request.class, parallelism)?);
            request.memory_reservation_bytes = Some(estimate_memory_reservation(
                config,
                &request.class,
                parallelism,
            )?);
        }
        request.memory_cargo_jobs = jobs;
        request.memory_rust_test_threads = threads;
        request.memory_parallelism = Some(parallelism);
        let mut arguments = request.command.get(1..).unwrap_or_default().to_vec();
        request.lease_record =
            Some(prepare_request_environment(config, request, &mut arguments, active_units).record);
    }
    request.observe_blocks(Vec::new(), clock);
    request.phase = Phase::Running;
    request.started_at_ms = Some(now);
    request.started_clock_ms = Some(clock);
    request.heartbeat_at_ms = Some(now);
    request.reason = None;
    state.event(id, config, now, clock)
}

#[cfg(test)]
enum Admission {
    Stopped,
    Waiting,
    Granted,
}

#[cfg(test)]
fn admit_worker(
    store: &Store,
    id: &RequestId,
    fence: &WorkerFence,
    interrupted: &AtomicBool,
) -> Result<Admission> {
    admit_worker_with_clock(
        store,
        id,
        fence,
        interrupted,
        || Ok((now_ms()?, clock_ms()?)),
        |config, now| config.sample(now).ok(),
    )
}

#[cfg(test)]
fn admit_worker_at(
    store: &Store,
    id: &RequestId,
    fence: &WorkerFence,
    interrupted: &AtomicBool,
    now: u64,
    clock: u64,
    sample_for: impl FnOnce(&Config, u64) -> Option<HostSample>,
) -> Result<Admission> {
    admit_worker_with_clock(
        store,
        id,
        fence,
        interrupted,
        || Ok((now, clock)),
        sample_for,
    )
}

#[cfg(test)]
fn admit_worker_with_clock(
    store: &Store,
    id: &RequestId,
    fence: &WorkerFence,
    interrupted: &AtomicBool,
    read_clock: impl FnMut() -> Result<(u64, u64)>,
    sample_for: impl FnOnce(&Config, u64) -> Option<HostSample>,
) -> Result<Admission> {
    store.transaction_with_clock(read_clock, |state, config, now, clock| {
        let request = state.find(id)?;
        if request.phase != Phase::Queued || !fence.owns(request) {
            return Ok(Admission::Stopped);
        }
        if interrupted.load(Ordering::Relaxed) {
            state.find_mut(id)?.stop = Some(StopReason::Cancelled);
            state.finish(id, Completion::default(), config, now, clock)?;
            return Ok(Admission::Stopped);
        }
        while refuse_impossible_head(state, config, now, clock)? {}
        if state.find(id)?.phase.terminal() {
            return Ok(Admission::Stopped);
        }
        let sample = sample_for(config, now);
        let reason = state.admission_reason(id, config, sample.as_ref(), now, clock)?;
        let blockers = state.block_observations(id, config, sample.as_ref(), now, clock)?;
        let position = state.queue_position(id, config, clock);
        let changed = state.find_mut(id)?.observe_blocks(blockers, clock);
        state.find_mut(id)?.trace.position = position;
        {
            let request = state.find_mut(id)?;
            request.heartbeat_at_ms = Some(now);
            request.reason = reason.map(str::to_owned);
        }
        let blocked = current_block_reason(state, config, sample.as_ref(), now, clock)?;
        state.metrics.observe_block(blocked, now, clock)?;
        if reason.is_some() {
            if changed {
                state.event(id, config, now, clock)?;
                if let Some(event) = state.events.last_mut() {
                    event.kind = "blocked".to_owned();
                }
            }
            return Ok(Admission::Waiting);
        }
        if refuse_impossible_request(state, config, id, now, clock)? {
            return Ok(Admission::Stopped);
        }
        grant_request(state, config, id, now, clock)?;
        Ok(Admission::Granted)
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

pub(crate) fn worker(store: &Store, id: &RequestId, generation: Option<u64>) -> Result<()> {
    let Some(_request_lease) = store.inherited_or_try_lock(&format!("{}.spawn.lock", id.0))? else {
        return Ok(());
    };
    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, interrupted.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())?;
    let identity = Processes::self_identity()?;
    if u32::try_from(nix::unistd::getpgrp().as_raw()).ok() != Some(identity.pid) {
        return Err(Error::new(
            ErrorCode::State,
            "supervisor must own an isolated process group",
        ));
    }
    let fence = match generation {
        Some(generation) => claim_spawned_worker(store, id, &identity, generation)?,
        None => claim_worker(store, id, &identity)?,
    };
    let Some(fence) = fence else {
        return Ok(());
    };
    if generation.is_none() {
        store.transaction(|state, _| {
            state.find_mut(id)?.pid_namespace = crate::process::pid_namespace()?;
            store.write_claim(&crate::model::WorkerClaim {
                protocol: 2,
                store_id: state.store_id.clone(),
                request_id: id.clone(),
                generation: fence.generation,
                launcher: identity.clone(),
                pid_namespace: crate::process::pid_namespace()?,
                worker: Some(identity.clone()),
            })
        })?;
    }
    let mut last_snapshot = None;
    let request = loop {
        if store.try_admission_lock()?.is_none() {
            let (state, _) = store.snapshot()?;
            let request = state.find(id)?;
            if !fence.owns(request) || request.phase.terminal() {
                return Ok(());
            }
            if request.phase == Phase::Queued {
                store.transaction(|state, _| {
                    if state.find(id)?.phase == Phase::Queued && fence.owns(state.find(id)?) {
                        let mut claim = store.claim(id)?.ok_or_else(|| {
                            Error::new(ErrorCode::State, "queued handoff claim missing")
                        })?;
                        claim.worker = None;
                        store.write_claim(&claim)?;
                    }
                    Ok(())
                })?;
                // Keep the spawn lock, PID namespace, caller environment and
                // descriptors. The successor re-registers this same generation.
                while store.try_admission_lock()?.is_none() {
                    std::thread::sleep(Duration::from_millis(wait_interval()));
                }
                let error = Command::new(store.service_binary()?)
                    .arg("--state")
                    .arg(&store.path)
                    .arg("--config")
                    .arg(&store.config_path)
                    .arg("worker")
                    .arg(&id.0)
                    .arg(fence.generation.to_string())
                    .stdin(Stdio::from(_request_lease))
                    .exec();
                return Err(error.into());
            }
        }
        if interrupted.load(Ordering::Relaxed) {
            store.transaction(|state, config| {
                if fence.owns(state.find(id)?) && state.find(id)?.phase == Phase::Queued {
                    state.find_mut(id)?.stop = Some(StopReason::Cancelled);
                    state.finish(id, Completion::default(), config, now_ms()?, clock_ms()?)?;
                }
                Ok(())
            })?;
        }
        let notice = store.notice(id).ok().flatten();
        if last_snapshot
            .is_none_or(|last: std::time::Instant| last.elapsed() >= Duration::from_secs(2))
            || notice.as_ref().is_none_or(|notice| {
                notice.phase != Phase::Queued || notice.generation != fence.generation
            })
        {
            let (state, _) = store.snapshot()?;
            let request = state.find(id)?;
            if !fence.owns(request) || request.phase.terminal() {
                return Ok(());
            }
            if request.phase == Phase::Running {
                break request.clone();
            }
            last_snapshot = Some(std::time::Instant::now());
        }
        crate::scheduler::ensure(store)?;
        std::thread::sleep(Duration::from_millis(wait_interval()));
    };
    let (watchdog, mut watchdog_child) =
        match crate::watchdog::launch(store, id, &identity, fence.generation) {
            Ok(watchdog) => watchdog,
            Err(error) => {
                store.transaction(|state, config| {
                    if fence.owns(state.find(id)?) {
                        state.finish(
                            id,
                            Completion {
                                reason: Some("supervision_lost"),
                                ..Completion::default()
                            },
                            config,
                            now_ms()?,
                            clock_ms()?,
                        )?;
                    }
                    Ok(())
                })?;
                return Err(error);
            }
        };
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
    let child = match start_command(store, request, fence, watchdog) {
        Ok(CommandStart::Started(child)) => child,
        Ok(CommandStart::Refused) => {
            store.transaction(|state, config| {
                finish_refused_command(state, config, &request.id, fence)
            })?;
            return Ok(());
        }
        Err(error) => {
            let grace_ms = Config::read(&store.config_path)
                .map(|policy| policy.termination_grace_ms)
                .unwrap_or(10_000);
            terminate_supervision_group(watchdog_child, None, watchdog.pid, grace_ms)?;
            let reason = if error.code == ErrorCode::TargetUnavailable {
                "target_unavailable"
            } else {
                "supervision_lost"
            };
            eprintln!("request={} reason={reason} detail={error}", request.id.0);
            store.transaction(|state, config| {
                if fence.owns(state.find(&request.id)?) {
                    state.find_mut(&request.id)?.supervision_detail =
                        Some(error.message.to_string());
                    state.finish(
                        &request.id,
                        Completion {
                            reason: Some(reason),
                            ..Completion::default()
                        },
                        config,
                        now_ms()?,
                        clock_ms()?,
                    )?;
                }
                Ok(())
            })?;
            return Err(error);
        }
    };
    supervise_started_command(
        store,
        request,
        fence,
        watchdog,
        watchdog_child,
        interrupted,
        child,
        |evidence, processes, group| evidence.observe_group(processes, group),
    )
}

fn finish_refused_command(
    state: &mut crate::model::State,
    config: &Config,
    id: &RequestId,
    fence: &WorkerFence,
) -> Result<()> {
    fence.require_running(state.find(id)?)?;
    state.finish(
        id,
        Completion {
            reason: Some("memory_cap_unavailable"),
            ..Completion::default()
        },
        config,
        now_ms()?,
        clock_ms()?,
    )?;
    state.find_mut(id)?.trace.lifecycle_state = "refused".to_owned();
    if let Some(event) = state.events.last_mut() {
        event.kind = "refused".to_owned();
        event.trace.lifecycle_state = "refused".to_owned();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn supervise_started_command(
    store: &Store,
    request: &Request,
    fence: &WorkerFence,
    watchdog: &crate::model::ProcessIdentity,
    watchdog_child: &mut Child,
    interrupted: &AtomicBool,
    mut child: Child,
    observe_oom: impl FnMut(&mut CgroupOomEvidence, &Processes, u32),
) -> Result<()> {
    let grace_ms = Config::read(&store.config_path)
        .map(|policy| policy.termination_grace_ms)
        .unwrap_or(10_000);
    let result = supervise_command_loop(
        store,
        request,
        fence,
        watchdog,
        watchdog_child,
        interrupted,
        &mut child,
        observe_oom,
    )
    .and_then(|()| {
        if let Some(outcome) = child.try_wait()? {
            store.transaction(|state, config| {
                reconcile_orphan_outcome(
                    state,
                    config,
                    &request.id,
                    fence,
                    CommandOutcome {
                        code: outcome.code(),
                        signal: outcome.signal(),
                    },
                )
            })?;
            Ok(())
        } else {
            Err(Error::new(
                ErrorCode::State,
                "lease ended while command remained alive",
            ))
        }
    });
    if let Err(error) = result {
        terminate_supervision_group(watchdog_child, Some(&mut child), watchdog.pid, grace_ms)?;
        let outcome = child.wait()?;
        eprintln!(
            "request={} reason=supervision_lost detail={error}",
            request.id.0
        );
        store.transaction(|state, config| {
            if fence.owns(state.find(&request.id)?) && !state.find(&request.id)?.phase.terminal() {
                state.find_mut(&request.id)?.supervision_detail = Some(error.message.to_string());
                state.finish(
                    &request.id,
                    Completion {
                        code: outcome.code(),
                        signal: outcome.signal(),
                        reason: Some("supervision_lost"),
                    },
                    config,
                    now_ms()?,
                    clock_ms()?,
                )?;
            } else if state.find(&request.id)?.phase.terminal()
                && (fence.owns(state.find(&request.id)?)
                    || state.find(&request.id)?.worker.is_none())
                && state.find(&request.id)?.reason.as_deref() != Some("rss_cap")
            {
                // An erroneously expired lease can still own live children.
                // Correct its earlier success/reboot record after cleanup.
                let current = state.find_mut(&request.id)?;
                current.supervision_detail = Some(error.message.to_string());
                current.phase = Phase::Running;
                state.finish(
                    &request.id,
                    Completion {
                        code: outcome.code(),
                        signal: outcome.signal(),
                        reason: Some("supervision_lost"),
                    },
                    config,
                    now_ms()?,
                    clock_ms()?,
                )?;
                if let Some(event) = state.events.last_mut() {
                    event.correction = true;
                }
            }
            Ok(())
        })?;
        return Err(error);
    }
    Ok(())
}

pub(crate) fn terminate_supervision_group(
    anchor: &mut Child,
    mut command: Option<&mut Child>,
    group: u32,
    grace_ms: u64,
) -> Result<()> {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if anchor.id() != group {
        return Err(Error::new(
            ErrorCode::State,
            "cleanup group is not the owned watchdog child",
        ));
    }
    if !crate::process::child_is_owned(anchor)? {
        // Only reap after all live members exit; never signal a reused PGID.
        return Ok(());
    }
    let group =
        i32::try_from(group).map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
    let group = Pid::from_raw(group);
    let command_group = if let Some(child) = command.as_deref() {
        if crate::process::child_is_owned(child)? && !crate::process::owned_child_exited(child)? {
            let child_pid = i32::try_from(child.id())
                .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
            nix::unistd::getpgid(Some(Pid::from_raw(child_pid)))
                .ok()
                .filter(|actual| actual.as_raw() == child_pid && *actual != group)
        } else {
            None
        }
    } else {
        None
    };
    let started = std::time::Instant::now();
    let mut killed = false;
    loop {
        let signal = if started.elapsed() >= Duration::from_millis(grace_ms) {
            Signal::SIGKILL
        } else {
            Signal::SIGTERM
        };
        if !killed {
            for target in std::iter::once(group).chain(command_group) {
                match killpg(target, signal) {
                    Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                    Err(error) => {
                        // Keep ownership and occupancy rather than abandon
                        // live children if signaling is denied.
                        eprintln!("supervision_lost: group cleanup signal failed: {error}");
                    }
                }
            }
        }
        if signal == Signal::SIGKILL && !killed {
            // Reap the pinned group leader only after killing the whole group.
            let _ = anchor.kill();
            anchor.wait()?;
            if let Some(child) = command.as_deref_mut() {
                let _ = child.kill();
                child.wait()?;
            }
            killed = true;
        }
        if group_absent(
            u32::try_from(group.as_raw())
                .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?,
        ) && command_group
            .is_none_or(|target| u32::try_from(target.as_raw()).is_ok_and(group_absent))
        {
            let _ = anchor.kill();
            anchor.wait()?;
            if let Some(child) = command.as_deref_mut() {
                let _ = child.kill();
                child.wait()?;
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[allow(clippy::too_many_arguments)]
fn supervise_command_loop(
    store: &Store,
    request: &Request,
    fence: &WorkerFence,
    watchdog: &crate::model::ProcessIdentity,
    watchdog_child: &mut Child,
    interrupted: &AtomicBool,
    child: &mut Child,
    mut observe_oom: impl FnMut(&mut CgroupOomEvidence, &Processes, u32),
) -> Result<()> {
    let enforcement = persisted_memory_enforcement(store, &request.id)?;
    let command_root = store
        .snapshot()?
        .0
        .find(&request.id)?
        .lease_record
        .as_ref()
        .and_then(|record| record.command.clone());
    let scope = store
        .snapshot()?
        .0
        .find(&request.id)?
        .lease_record
        .as_ref()
        .and_then(|record| record.memory_scope.clone());
    let mut cgroup_oom_evidence = CgroupOomEvidence {
        scope: scope.clone(),
        ..Default::default()
    };
    let mut status = None;
    let mut degraded = false;
    let mut stop_degraded_group = false;
    let mut watchdog_reaped = false;
    let grace = Config::read(&store.config_path)?.termination_grace_ms;
    loop {
        let watchdog_exited =
            watchdog_reaped || crate::process::owned_child_exited(watchdog_child)?;
        let mut scope_rss = None;
        if enforcement == Some(MemoryEnforcement::SystemdScope) {
            let processes = Processes::read();
            scope_rss = command_root
                .as_ref()
                .and_then(|root| processes.command_rss_bytes(root));
            if let Some(root) = &command_root {
                cgroup_oom_evidence
                    .event_files
                    .extend(processes.command_memory_events_files(root));
                cgroup_oom_evidence.retain_owned_scope();
            }
            observe_oom(&mut cgroup_oom_evidence, &processes, watchdog.pid);
        }
        if status.is_none() {
            status = child.try_wait()?;
        }
        if status.is_some() && enforcement == Some(MemoryEnforcement::SystemdScope) {
            observe_oom(&mut cgroup_oom_evidence, &Processes::read(), watchdog.pid);
            cgroup_oom_evidence.read_known_files();
            let evidence_path = store.path.join(format!("{}.scope-oom.json", request.id.0));
            if let Ok(bytes) = std::fs::read(evidence_path) {
                let evidence: crate::scope_command::OomEvidence = serde_json::from_slice(&bytes)?;
                status = Some(evidence.command_status()?);
                cgroup_oom_evidence.oom_kill_count = cgroup_oom_evidence
                    .oom_kill_count
                    .max(evidence.oom_kill_count);
                cgroup_oom_evidence.detected |= evidence.oom_kill_count > 0;
                if let Some(peak) = evidence.memory_peak_bytes {
                    cgroup_oom_evidence.peak_bytes =
                        Some(cgroup_oom_evidence.peak_bytes.unwrap_or(0).max(peak));
                }
            }
            if let Some(scope) = &scope {
                cgroup_oom_evidence.observe_scope_result(scope);
            }
        }
        if status.is_none()
            && cgroup_oom_evidence.scope_vanished()
            && !cgroup_oom_evidence.detected()
        {
            return Err(Error::new(
                ErrorCode::State,
                "memory scope vanished while command is running",
            ));
        }
        if status.is_some() && watchdog_exited && !watchdog_reaped {
            // Clean up and reap outside the queue lock. A missed descendant
            // must never freeze cancellation/admission while we await ESRCH.
            terminate_supervision_group(watchdog_child, Some(child), watchdog.pid, grace)?;
            watchdog_reaped = true;
        }
        let policy = store.transaction(|state, config| {
            if state.find(&request.id)?.phase.terminal() {
                if let Some(outcome) = status {
                    reconcile_orphan_outcome(
                        state,
                        config,
                        &request.id,
                        fence,
                        CommandOutcome {
                            code: outcome.code(),
                            signal: outcome.signal(),
                        },
                    )?;
                }
                return Ok(None);
            }
            fence.require_running(state.find(&request.id)?)?;
            let current = state.find_mut(&request.id)?;
            if let Some(bytes) = scope_rss {
                current.measured_memory_bytes =
                    Some(current.measured_memory_bytes.unwrap_or(0).max(bytes));
            }
            // Persist OOM evidence before publishing command_finished. The
            // watchdog may win finalization as soon as that outcome is visible.
            record_scope_oom(
                current,
                enforcement,
                cgroup_oom_evidence.oom_kill_count,
                cgroup_oom_evidence.scope_oom_killed,
            );
            record_scope_peak(current, &cgroup_oom_evidence);
            if let Some(outcome) = status {
                current.command_finished = Some(CommandOutcome {
                    code: outcome.code(),
                    signal: outcome.signal(),
                });
            } else if watchdog_exited
                || Processes::read().role_identity(watchdog, &request.id, "watchdog", watchdog.pid)
                    != IdentityState::Matching
            {
                degraded = true;
                if current.reason.as_deref() != Some("supervision_lost") {
                    current.reason = Some("supervision_degraded".to_owned());
                }
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
            stop_degraded_group = degraded && current.stop.is_some();
            if degraded
                && current.stop.is_none()
                && let Some(cap) = current.memory_cap_bytes
                && let Some(sample) = current
                    .lease_record
                    .as_ref()
                    .and_then(|record| record.command.as_ref())
                    .and_then(|root| Processes::read().command_memory_sample(root))
            {
                crate::watchdog::observe_memory_sample(current, sample, cap);
                stop_degraded_group = current.stop.is_some();
            }
            if let Some(outcome) = status {
                if !group_absent(watchdog.pid) {
                    return Ok(Some(crate::config::POLL_INTERVAL_MS));
                }
                if enforcement == Some(MemoryEnforcement::SystemdScope) {
                    cgroup_oom_evidence.read_known_files();
                    record_scope_oom(
                        state.find_mut(&request.id)?,
                        enforcement,
                        cgroup_oom_evidence.oom_kill_count,
                        cgroup_oom_evidence.scope_oom_killed,
                    );
                    record_scope_peak(state.find_mut(&request.id)?, &cgroup_oom_evidence);
                }
                finish_supervised_command(
                    state,
                    config,
                    &request.id,
                    fence,
                    watchdog.pid,
                    Completion {
                        code: outcome.code(),
                        signal: outcome.signal(),
                        reason: cgroup_cap_kill_reason(enforcement, cgroup_oom_evidence.detected())
                            .or(if degraded && outcome.code() == Some(0) {
                                Some("supervision_degraded")
                            } else {
                                None
                            }),
                    },
                )?;
                return Ok(None);
            }
            Ok(Some(crate::config::POLL_INTERVAL_MS))
        })?;
        let Some(poll_ms) = policy else {
            return Ok(());
        };
        if stop_degraded_group {
            // The command may have moved into its own group. Both Child
            // handles pin ownership even after watchdog supervision was lost.
            terminate_supervision_group(watchdog_child, Some(child), watchdog.pid, grace)?;
            watchdog_reaped = true;
        }
        std::thread::sleep(Duration::from_millis(poll_ms));
    }
}

fn reconcile_orphan_outcome(
    state: &mut crate::model::State,
    config: &Config,
    id: &RequestId,
    fence: &WorkerFence,
    outcome: CommandOutcome,
) -> Result<()> {
    let current = state.find_mut(id)?;
    if !fence.owns(current)
        || current.stop.is_some()
        || current.reason.as_deref() != Some("supervisor_lost")
    {
        return Ok(());
    }
    // The direct parent's owned Child result outranks a watchdog's inference.
    // This race occurs only after the client is gone and the command has exited.
    current.phase = Phase::Running;
    current.reason = None;
    current.command_finished = Some(outcome.clone());
    state.finish(
        id,
        Completion {
            code: outcome.code,
            signal: outcome.signal,
            reason: (outcome.code == Some(0)).then_some("supervision_degraded"),
        },
        config,
        now_ms()?,
        clock_ms()?,
    )?;
    if let Some(event) = state.events.last_mut() {
        event.correction = true;
    }
    Ok(())
}

fn persisted_memory_enforcement(
    store: &Store,
    id: &RequestId,
) -> Result<Option<MemoryEnforcement>> {
    store.transaction(|state, _| {
        Ok(state
            .find(id)?
            .lease_record
            .as_ref()
            .map(|record| record.memory_enforcement))
    })
}

fn finish_supervised_command(
    state: &mut crate::model::State,
    config: &Config,
    id: &RequestId,
    fence: &WorkerFence,
    group: u32,
    completion: Completion,
) -> Result<()> {
    finish_owned(state, config, id, fence, group, completion)
}

fn start_command(
    store: &Store,
    request: &Request,
    fence: &WorkerFence,
    watchdog: &crate::model::ProcessIdentity,
) -> Result<CommandStart> {
    let (config_snapshot, memory_cap) = store.transaction(|state, config| {
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
        Ok((config.clone(), request.memory_cap_bytes))
    })?;
    let prepared_cache = crate::kache_wrapper::Prepared::discover(&config_snapshot.cache);
    let (mut enforcement, probe_launch_spec) = select_lease_launch(
        LauncherPlatform::current(),
        request
            .command
            .first()
            .ok_or_else(|| Error::new(ErrorCode::State, "persisted empty command"))?,
        request.command.get(1..).unwrap_or_default(),
        memory_cap,
        config_snapshot.on_missing_cgroup,
        |cap| systemd_scope_properties(cap, Duration::from_secs(2)),
    );
    if enforcement == MemoryEnforcement::RssWatchdog && !Processes::read().can_sample_memory() {
        enforcement = MemoryEnforcement::Refused;
    }

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
        let mut arguments = request.command[1..].to_vec();
        let environment =
            prepare_request_environment(config, request, &mut arguments, active_units);
        let memory_cap = request.memory_cap_bytes;
        let mut lease_record = environment.record;
        lease_record.memory_enforcement = enforcement;
        if enforcement == MemoryEnforcement::Refused {
            state.find_mut(&request.id)?.lease_record = Some(lease_record);
            return Ok(CommandStart::Refused);
        }
        let scope_helper;
        let scope_arguments;
        let (launch_program, launch_arguments) = if enforcement == MemoryEnforcement::SystemdScope
            && probe_launch_spec.scope_properties == Some(ScopeProperties::MemoryOnly)
        {
            scope_helper = std::env::current_exe()?;
            scope_arguments = vec![
                "scope-command".into(),
                "--evidence".into(),
                store
                    .path
                    .join(format!("{}.scope-oom.json", request.id.0))
                    .to_string_lossy()
                    .into_owned(),
                "--".into(),
                program.clone(),
            ]
            .into_iter()
            .chain(arguments.iter().cloned())
            .collect::<Vec<_>>();
            (
                scope_helper.to_str().ok_or_else(|| {
                    Error::new(ErrorCode::State, "scope helper path is not UTF-8")
                })?,
                scope_arguments.as_slice(),
            )
        } else {
            (program.as_str(), arguments.as_slice())
        };
        let launch_spec = lease_command_spec_for_enforcement(
            LauncherPlatform::current(),
            launch_program,
            launch_arguments,
            memory_cap,
            enforcement,
            probe_launch_spec.scope_properties,
        );
        let mut command = Command::new(launch_spec.program);
        if enforcement == MemoryEnforcement::SystemdScope {
            let scope = format!("ix-build-{}.scope", crate::store::random_id()?);
            command.arg(format!("--unit={scope}"));
            lease_record.memory_scope = Some(scope);
        }
        command
            .stdin(std::process::Stdio::null())
            .args(launch_spec.arguments)
            .current_dir(&request.cwd)
            .process_group(group);
        if let Some(real_cargo) = &config.real_cargo {
            command.env("IX_BUILD_REAL_CARGO", real_cargo);
        }
        if let Some(value) = environment.cargo_build_jobs_env {
            command.env("CARGO_BUILD_JOBS", value);
        }
        if let Some(value) = environment.rust_test_threads_env {
            command.env("RUST_TEST_THREADS", value);
        }
        let (incremental, incremental_overwrote_caller) = apply_incremental_environment(
            &mut command,
            config,
            request.caller_cargo_incremental.as_deref(),
        );
        let (target_dir, target_overwrote_caller) =
            apply_target_environment(&mut command, config, request)
                .map_err(|error| Error::new(ErrorCode::TargetUnavailable, error.message))?;
        lease_record.cargo_incremental = incremental;
        lease_record.cargo_incremental_overwrote_caller = incremental_overwrote_caller;
        lease_record.cargo_target_dir = target_dir;
        lease_record.cargo_target_dir_overwrote_caller = target_overwrote_caller;
        let (cache_wrapper, cache_wrapper_status) =
            prepared_cache.apply(&mut command, &config.cache);
        lease_record.cache_wrapper = cache_wrapper;
        lease_record.cache_wrapper_status = cache_wrapper_status;
        let child = command.spawn()?;
        // This owned child was just born and may already have moved groups.
        // Read its known PID directly instead of relying on a host-wide census.
        lease_record.command = Processes::read_workers(&[child.id()]).member_identity(child.id());
        state.find_mut(&request.id)?.lease_record = Some(lease_record);
        let running = state.find_mut(&request.id)?;
        running.command_started_clock_ms = Some(clock_ms()?);
        running.trace.running_at_ms = Some(now_ms()?);
        state.event(&request.id, config, now_ms()?, clock_ms()?)?;
        if let Some(event) = state.events.last_mut() {
            event.kind = "command_started".to_owned();
        }
        Ok(CommandStart::Started(child))
    })
}

enum CommandStart {
    Started(Child),
    Refused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LauncherPlatform {
    Linux,
    #[cfg(any(not(target_os = "linux"), test))]
    Other,
}

impl LauncherPlatform {
    fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self::Other
        }
    }
}

struct LaunchSpec {
    program: std::ffi::OsString,
    arguments: Vec<std::ffi::OsString>,
    scope_properties: Option<ScopeProperties>,
}

fn lease_command_spec(
    platform: LauncherPlatform,
    program: &str,
    arguments: &[String],
    memory_cap_bytes: Option<u64>,
    scope_properties: Option<ScopeProperties>,
) -> LaunchSpec {
    if platform == LauncherPlatform::Linux
        && let Some(memory_cap_bytes) = memory_cap_bytes
    {
        let mut command_arguments = vec![
            "--user".into(),
            "--scope".into(),
            "--quiet".into(),
            "-p".into(),
            format!("MemoryMax={memory_cap_bytes}").into(),
            "-p".into(),
            "MemorySwapMax=0".into(),
        ];
        if scope_properties == Some(ScopeProperties::Full) {
            command_arguments.extend(["-p".into(), "OOMPolicy=stop".into()]);
        }
        command_arguments.extend(["--".into(), program.into()]);
        command_arguments.extend(arguments.iter().map(Into::into));
        LaunchSpec {
            program: "systemd-run".into(),
            arguments: command_arguments,
            scope_properties,
        }
    } else {
        LaunchSpec {
            program: program.into(),
            arguments: arguments.iter().map(Into::into).collect(),
            scope_properties: None,
        }
    }
}

fn lease_command_spec_for_enforcement(
    platform: LauncherPlatform,
    program: &str,
    arguments: &[String],
    memory_cap_bytes: Option<u64>,
    enforcement: MemoryEnforcement,
    scope_properties: Option<ScopeProperties>,
) -> LaunchSpec {
    let scope_cap = (enforcement == MemoryEnforcement::SystemdScope)
        .then_some(memory_cap_bytes)
        .flatten();
    lease_command_spec(platform, program, arguments, scope_cap, scope_properties)
}

fn systemd_scope_properties(memory_cap_bytes: u64, timeout: Duration) -> Option<ScopeProperties> {
    if !memory_controller_delegated() {
        return None;
    }
    crate::scope_properties::cached(|properties| {
        let spec = lease_command_spec(
            LauncherPlatform::Linux,
            "/usr/bin/true",
            &[],
            Some(memory_cap_bytes),
            Some(properties),
        );
        run_probe_with_timeout(timeout, || {
            Command::new(spec.program)
                .args(spec.arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        })
    })
    .unwrap_or_else(|error| {
        eprintln!("scope capability cache unavailable: {error}");
        None
    })
}

fn memory_controller_delegated() -> bool {
    let Ok(membership) = std::fs::read_to_string("/proc/self/cgroup") else {
        return false;
    };
    let Some(relative) = membership.lines().find_map(|line| line.strip_prefix("0::")) else {
        return false;
    };
    memory_controller_delegated_at(std::path::Path::new("/sys/fs/cgroup"), relative)
}

fn memory_controller_delegated_at(cgroup_root: &std::path::Path, relative: &str) -> bool {
    let relative = std::path::Path::new(relative.trim_start_matches('/'));
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return false;
    }
    let group = cgroup_root.join(relative);
    if !group.join("memory.max").is_file() {
        return false;
    }
    let Some(parent) = group.parent() else {
        return false;
    };
    std::fs::read_to_string(parent.join("cgroup.subtree_control"))
        .is_ok_and(|controllers| controllers.split_whitespace().any(|item| item == "memory"))
}

fn run_probe_with_timeout(
    timeout: Duration,
    spawn: impl FnOnce() -> std::io::Result<Child>,
) -> bool {
    let Ok(mut child) = spawn() else {
        return false;
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn select_memory_enforcement(
    platform: LauncherPlatform,
    memory_cap_bytes: Option<u64>,
    on_missing: crate::config::OnMissingCgroup,
    mut systemd_scope_probe: impl FnMut(u64) -> bool,
) -> MemoryEnforcement {
    let Some(memory_cap_bytes) = memory_cap_bytes else {
        return MemoryEnforcement::NotConfigured;
    };
    if platform == LauncherPlatform::Linux && systemd_scope_probe(memory_cap_bytes) {
        return MemoryEnforcement::SystemdScope;
    }
    match on_missing {
        crate::config::OnMissingCgroup::Refuse => MemoryEnforcement::Refused,
        crate::config::OnMissingCgroup::RssWatchdog => MemoryEnforcement::RssWatchdog,
    }
}

fn select_lease_launch(
    platform: LauncherPlatform,
    program: &str,
    arguments: &[String],
    memory_cap_bytes: Option<u64>,
    on_missing: crate::config::OnMissingCgroup,
    mut probe: impl FnMut(u64) -> Option<ScopeProperties>,
) -> (MemoryEnforcement, LaunchSpec) {
    let mut properties = None;
    let enforcement = select_memory_enforcement(platform, memory_cap_bytes, on_missing, |cap| {
        properties = probe(cap);
        properties.is_some()
    });
    let launch = lease_command_spec_for_enforcement(
        platform,
        program,
        arguments,
        memory_cap_bytes,
        enforcement,
        properties,
    );
    (enforcement, launch)
}

fn cgroup_cap_kill_reason(
    enforcement: Option<MemoryEnforcement>,
    oom_kill_observed: bool,
) -> Option<&'static str> {
    (enforcement == Some(MemoryEnforcement::SystemdScope) && oom_kill_observed).then_some("rss_cap")
}

fn record_scope_oom(
    request: &mut Request,
    enforcement: Option<MemoryEnforcement>,
    count: u64,
    scope_oom_killed: bool,
) {
    if let Some(record) = &mut request.lease_record {
        record.oom_kill_count = record.oom_kill_count.max(count);
        record.scope_oom_killed |= scope_oom_killed;
    }
    if request.stop.is_none()
        && cgroup_cap_kill_reason(enforcement, count > 0 || scope_oom_killed).is_some()
    {
        request.stop = Some(StopReason::MemoryCap);
    }
}

fn record_scope_peak(request: &mut Request, evidence: &CgroupOomEvidence) {
    if let (Some(record), Some(bytes)) = (&mut request.lease_record, evidence.peak_bytes) {
        record.memory_sample = Some(crate::model::MemorySample {
            contributors: Vec::new(),
            bytes,
            metric: crate::model::MemoryMetric::CgroupMemoryPeak,
        });
    }
}

#[derive(Default)]
struct CgroupOomEvidence {
    scope_oom_killed: bool,
    scope: Option<String>,
    oom_kill_count: u64,
    event_files: std::collections::BTreeSet<std::path::PathBuf>,
    detected: bool,
    peak_bytes: Option<u64>,
}

fn scope_result_output(scope: &str) -> Option<std::process::Output> {
    let mut child = Command::new("systemctl")
        .args(["--user", "show", scope, "-p", "Result", "-p", "MemoryPeak"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return child.wait_with_output().ok().filter(|_| status.success()),
            Ok(None) if started.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

impl CgroupOomEvidence {
    fn retain_owned_scope(&mut self) {
        if let Some(scope) = &self.scope {
            self.event_files.retain(|path| {
                path.parent()
                    .and_then(|parent| parent.file_name())
                    .is_some_and(|name| name == scope.as_str())
            });
        }
    }
    fn observe_scope_result(&mut self, scope: &str) {
        // Failed scopes retain Result=oom-kill even if systemd already removed
        // their cgroup before the parent's first process snapshot completed.
        if let Some(output) = scope_result_output(scope) {
            let text = String::from_utf8_lossy(&output.stdout);
            self.observe_scope_text(&text);
        }
    }

    fn observe_scope_text(&mut self, text: &str) {
        if text.lines().any(|line| line == "Result=oom-kill") {
            self.scope_oom_killed = true;
            self.detected = true;
            if let Some(peak) = text
                .lines()
                .find_map(|line| line.strip_prefix("MemoryPeak="))
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value != u64::MAX)
            {
                self.peak_bytes = Some(self.peak_bytes.unwrap_or(0).max(peak));
            }
        }
    }
    fn scope_vanished(&self) -> bool {
        !self.event_files.is_empty() && self.event_files.iter().all(|path| !path.is_file())
    }
    fn observe_group(&mut self, processes: &Processes, group: u32) {
        self.event_files
            .extend(processes.group_memory_events_files(group));
        self.retain_owned_scope();
        self.read_known_files();
    }

    #[cfg(test)]
    fn observe_file(&mut self, path: &std::path::Path) {
        self.event_files.insert(path.to_owned());
        self.read_known_files();
    }

    fn read_known_files(&mut self) {
        for path in &self.event_files {
            if let Some(count) = crate::process::oom_kill_count_from_file(path) {
                self.oom_kill_count = self.oom_kill_count.max(count);
            }
            if let Some(peak) = path
                .parent()
                .and_then(|parent| std::fs::read_to_string(parent.join("memory.peak")).ok())
                .and_then(|text| text.trim().parse::<u64>().ok())
            {
                self.peak_bytes = Some(self.peak_bytes.map_or(peak, |previous| previous.max(peak)));
            }
        }
        self.detected |= self.oom_kill_count > 0;
    }

    fn detected(&self) -> bool {
        self.detected
    }
}

struct PreparedBuildEnvironment {
    cargo_build_jobs_env: Option<String>,
    rust_test_threads_env: Option<String>,
    record: LeaseRecord,
}

fn prepare_build_environment(
    config: &crate::config::Config,
    arguments: &mut [String],
    caller_cargo_build_jobs: Option<&str>,
    caller_rust_test_threads: Option<&str>,
    active_units: u32,
) -> PreparedBuildEnvironment {
    let cargo_cli = clamp_argument(arguments, &["-j", "--jobs"], false, config.cargo_build_jobs);
    let rust_cli = clamp_test_threads(arguments, config.rust_test_threads);

    let (cargo_build_jobs_env, cargo_build_jobs, cargo_build_jobs_clamped) =
        if let Some(value) = cargo_cli {
            (
                value.value.map(|v| v.to_string()),
                value.value.and_then(to_u32),
                value.clamped,
            )
        } else if let Some(value) = caller_cargo_build_jobs {
            let (effective, numeric, clamped) = clamp_env_value(value, config.cargo_build_jobs);
            (Some(effective), numeric, clamped)
        } else if let Some(maximum_jobs) = config.cargo_build_jobs {
            let pressure_units = active_units.max(1);
            let scaled = (maximum_jobs / pressure_units).max(1);
            (Some(scaled.to_string()), Some(scaled), false)
        } else {
            let jobs = active_units.max(1);
            (Some(jobs.to_string()), Some(jobs), false)
        };

    let (rust_test_threads_env, rust_test_threads, rust_test_threads_clamped) =
        if let Some(value) = rust_cli {
            (
                value.value.map(|v| v.to_string()),
                value.value.and_then(to_u32),
                value.clamped,
            )
        } else if let Some(value) = caller_rust_test_threads {
            let (effective, numeric, clamped) = clamp_env_value(value, config.rust_test_threads);
            (Some(effective), numeric, clamped)
        } else if let Some(threads) = config.rust_test_threads {
            (Some(threads.to_string()), Some(threads), false)
        } else {
            (None, None, false)
        };

    PreparedBuildEnvironment {
        cargo_build_jobs_env,
        rust_test_threads_env,
        record: LeaseRecord {
            memory_sample: None,
            cargo_build_jobs,
            cargo_build_jobs_clamped,
            rust_test_threads,
            rust_test_threads_clamped,
            memory_enforcement: MemoryEnforcement::NotConfigured,
            ..LeaseRecord::default()
        },
    }
}

struct ArgumentLimit {
    value: Option<u64>,
    clamped: bool,
}

fn clamp_argument(
    arguments: &mut [String],
    options: &[&str],
    after_separator: bool,
    maximum: Option<u32>,
) -> Option<ArgumentLimit> {
    let separator = arguments.iter().position(|argument| argument == "--");
    let (start, end) = if after_separator {
        (separator?.saturating_add(1), arguments.len())
    } else {
        (0, separator.unwrap_or(arguments.len()))
    };
    for index in start..end {
        for option in options {
            if arguments[index] == *option {
                return Some(clamp_separate_argument(arguments, index, maximum));
            }
            if let Some(raw) = arguments[index].strip_prefix(&format!("{option}=")) {
                let parsed = raw.parse::<u64>().ok();
                let clamped =
                    parsed.is_some_and(|value| maximum.is_some_and(|max| value > u64::from(max)));
                if clamped {
                    arguments[index] = format!("{option}={}", maximum.unwrap_or_default());
                }
                return Some(ArgumentLimit {
                    value: parsed
                        .map(|value| maximum.map_or(value, |max| value.min(u64::from(max)))),
                    clamped,
                });
            }
            if *option == "-j"
                && let Some(raw) = arguments[index].strip_prefix("-j")
                && !raw.is_empty()
            {
                let raw = raw.strip_prefix('=').unwrap_or(raw);
                let parsed = raw.parse::<u64>().ok();
                let clamped =
                    parsed.is_some_and(|value| maximum.is_some_and(|max| value > u64::from(max)));
                if clamped {
                    arguments[index] = format!("-j{}", maximum.unwrap_or_default());
                }
                return Some(ArgumentLimit {
                    value: parsed
                        .map(|value| maximum.map_or(value, |max| value.min(u64::from(max)))),
                    clamped,
                });
            }
        }
    }
    None
}

fn clamp_separate_argument(
    arguments: &mut [String],
    index: usize,
    maximum: Option<u32>,
) -> ArgumentLimit {
    let Some(next) = arguments.get(index + 1) else {
        return ArgumentLimit {
            value: None,
            clamped: false,
        };
    };
    let parsed = next.parse::<u64>().ok();
    let clamped = parsed.is_some_and(|value| maximum.is_some_and(|max| value > u64::from(max)));
    if clamped {
        arguments[index + 1] = maximum.unwrap_or_default().to_string();
    }
    ArgumentLimit {
        value: parsed.map(|value| maximum.map_or(value, |max| value.min(u64::from(max)))),
        clamped,
    }
}

fn clamp_env_value(value: &str, maximum: Option<u32>) -> (String, Option<u32>, bool) {
    let parsed = value.parse::<u64>().ok();
    let clamped = parsed.is_some_and(|value| maximum.is_some_and(|max| value > u64::from(max)));
    let effective = parsed.map(|value| maximum.map_or(value, |max| value.min(u64::from(max))));
    let output = effective.map_or_else(|| value.to_owned(), |value| value.to_string());
    (output, effective.and_then(to_u32), clamped)
}

fn to_u32(value: u64) -> Option<u32> {
    u32::try_from(value).ok()
}

fn clamp_test_threads(arguments: &mut [String], maximum: Option<u32>) -> Option<ArgumentLimit> {
    let separator = arguments.iter().position(|argument| argument == "--");
    let start = separator.map_or(0, |index| index.saturating_add(1));
    for index in start..arguments.len() {
        if arguments[index] == "--test-threads" {
            return Some(clamp_separate_argument(arguments, index, maximum));
        }
        if let Some(raw) = arguments[index].strip_prefix("--test-threads=") {
            let parsed = raw.parse::<u64>().ok();
            let clamped =
                parsed.is_some_and(|value| maximum.is_some_and(|max| value > u64::from(max)));
            if clamped {
                arguments[index] = format!("--test-threads={}", maximum.unwrap_or_default());
            }
            return Some(ArgumentLimit {
                value: parsed.map(|value| maximum.map_or(value, |max| value.min(u64::from(max)))),
                clamped,
            });
        }
    }
    None
}

fn apply_incremental_environment(
    command: &mut Command,
    config: &crate::config::Config,
    caller_value: Option<&str>,
) -> (Option<String>, bool) {
    let policy_value = config
        .cargo_incremental
        .map(|incremental| if incremental { "1" } else { "0" });
    let effective = policy_value
        .map(str::to_owned)
        .or_else(|| caller_value.map(str::to_owned));
    if let Some(value) = &effective {
        command.env("CARGO_INCREMENTAL", value);
    } else {
        command.env_remove("CARGO_INCREMENTAL");
    }
    command.env("IX_BUILD_LEASE", "1");
    (effective, policy_value.is_some() && caller_value.is_some())
}

fn target_directory(
    config: &crate::config::Config,
    request: &Request,
    create: bool,
) -> Result<Option<std::path::PathBuf>> {
    let Some(configured_root) = &config.target_root else {
        return Ok(None);
    };
    let fallback;
    let configured_root = if create {
        match crate::writable::probe_directory(configured_root) {
            Ok(()) => configured_root,
            Err(error) if crate::writable::access_denied(&error) => {
                fallback = crate::writable::shared_root()?.join("targets");
                &fallback
            }
            Err(error) => return Err(error.into()),
        }
    } else {
        fallback = crate::writable::shared_root()?.join("targets");
        if request
            .lease_record
            .as_ref()
            .and_then(|lease| lease.cargo_target_dir.as_deref())
            .and_then(Path::parent)
            == Some(fallback.as_path())
        {
            &fallback
        } else {
            configured_root
        }
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
) -> Result<(Option<std::path::PathBuf>, bool)> {
    if let Some(caller_value) = &request.caller_cargo_target_dir {
        let target = std::path::PathBuf::from(caller_value);
        command.env("CARGO_TARGET_DIR", caller_value);
        apply_tmp_environment(command, &target, &request.cwd)?;
        return Ok((Some(target), false));
    }
    if let Some(target) = target_directory(config, request, true)? {
        command.env("CARGO_TARGET_DIR", &target);
        apply_tmp_environment(command, &target, &request.cwd)?;
        return Ok((Some(target), false));
    }
    command.env_remove("CARGO_TARGET_DIR");
    Ok((None, false))
}

fn apply_tmp_environment(command: &mut Command, target: &Path, cwd: &Path) -> Result<()> {
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        cwd.join(target)
    };
    let tmp = target.join(".tmp");
    std::fs::create_dir_all(&tmp)?;
    if std::fs::symlink_metadata(&tmp)?.file_type().is_symlink() {
        return Err(Error::new(
            ErrorCode::Configuration,
            "target temporary directory is a symlink",
        ));
    }
    command.env("TMPDIR", &tmp);
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
    let plan = store.transaction(|state, config| {
        let request = state.find(id)?;
        if request.caller_cargo_target_dir.is_some() {
            return Ok(None);
        }
        let still_active = state
            .requests
            .iter()
            .any(|candidate| candidate.owner == request.owner && candidate.phase == Phase::Running);
        if still_active {
            return Ok(None);
        }
        let Some(target) = target_directory(config, request, false)? else {
            return Ok(None);
        };
        if config.max_bytes.is_none() {
            return Ok(None);
        }
        Ok(Some(TargetCleanupPlan {
            owner: request.owner.clone(),
            target,
            max_bytes: config.max_bytes,
        }))
    })?;
    let Some(plan) = plan else {
        return Ok(());
    };
    let Some(inspection) = inspect_target_tree(&plan.target)? else {
        return Ok(());
    };
    let too_large =
        inspection.bound_reached || plan.max_bytes.is_some_and(|limit| inspection.bytes > limit);
    if !too_large {
        return Ok(());
    }

    let tombstone = store.transaction(|state, config| {
        if state
            .requests
            .iter()
            .any(|candidate| candidate.owner == plan.owner && candidate.phase == Phase::Running)
        {
            return Ok(None);
        }
        let request = state.find(id)?;
        let Some(confirmed) = target_directory(config, request, false)? else {
            return Ok(None);
        };
        if confirmed != plan.target {
            return Err(Error::new(
                ErrorCode::Configuration,
                "target directory changed before cleanup",
            ));
        }
        let parent = confirmed
            .parent()
            .ok_or_else(|| Error::new(ErrorCode::Configuration, "target has no parent"))?;
        let tombstone = parent.join(format!(".ix-build-cleanup-{}", id.0.replace(':', "-")));
        if tombstone.exists() {
            return Err(Error::new(
                ErrorCode::State,
                "target cleanup staging path already exists",
            ));
        }
        std::fs::rename(&confirmed, &tombstone)?;
        Ok(Some(tombstone))
    })?;
    if let Some(tombstone) = tombstone {
        std::fs::remove_dir_all(tombstone)?;
    }
    Ok(())
}

struct TargetCleanupPlan {
    owner: String,
    target: std::path::PathBuf,
    max_bytes: Option<u64>,
}

struct TargetInspection {
    bytes: u64,
    bound_reached: bool,
}

fn inspect_target_tree(target: &Path) -> Result<Option<TargetInspection>> {
    const MAX_ENTRIES: usize = 100_000;
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
    let mut visited = 0_usize;
    let mut bound_reached = false;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            visited = visited.saturating_add(1);
            if visited > MAX_ENTRIES {
                bound_reached = true;
                break;
            }
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
        if bound_reached {
            break;
        }
    }
    Ok(Some(TargetInspection {
        bytes,
        bound_reached,
    }))
}

#[cfg(test)]
pub(crate) fn prune(store: &Store) -> Result<usize> {
    prune_exported(store, false)
}

pub(crate) fn prune_exported(store: &Store, exported_only: bool) -> Result<usize> {
    let checkpoint = if exported_only {
        Some(serde_json::from_slice::<crate::model::ExportCheckpoint>(
            &std::fs::read(store.path.join("exported.json"))?,
        )?)
    } else {
        None
    };
    let terminal = store.transaction(|state, _| {
        if checkpoint
            .as_ref()
            .is_some_and(|ack| ack.store_id != state.store_id)
        {
            return Ok(Vec::new());
        }
        let terminal: Vec<_> = state
            .requests
            .iter()
            .filter(|request| {
                request.phase.terminal()
                    && checkpoint.as_ref().is_none_or(|ack| {
                        ack.store_id == state.store_id
                            && state
                                .events
                                .iter()
                                .filter(|e| e.request_id == request.id)
                                .all(|e| {
                                    e.event_id
                                        .rsplit_once(":e")
                                        .and_then(|(_, n)| n.parse::<u64>().ok())
                                        .is_some_and(|n| n <= ack.sequence)
                                })
                    })
            })
            .map(|request| request.id.clone())
            .collect();
        state
            .requests
            .retain(|request| !terminal.contains(&request.id));
        let previous_events = state.events.len();
        state.events.retain(|event| {
            !terminal.contains(&event.request_id)
                && !(checkpoint.as_ref().is_none_or(|ack| {
                    event
                        .event_id
                        .rsplit_once(":e")
                        .and_then(|(_, n)| n.parse::<u64>().ok())
                        .is_some_and(|n| n <= ack.sequence)
                }) && matches!(
                    event.kind.as_str(),
                    "supervisor_restart" | "recovered" | "boot_changed"
                ))
        });
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
        for extension in [
            "stdout",
            "stderr",
            "notice.json",
            "launch.json",
            "spawn.lock",
            "scope-oom.json",
        ] {
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
