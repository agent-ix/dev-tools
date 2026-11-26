// SPDX-License-Identifier: MIT
//! Persistent admission state and deterministic priority/FIFO decisions.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{Config, HostSample, MemoryAdmissionMode};
use crate::error::{Error, ErrorCode, Result};
use crate::metrics::MetricsState;

/// Globally random store identity plus monotonically increasing request number.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct RequestId(pub String);

fn default_request_class() -> String {
    "periodic".to_owned()
}

/// A supervisor identity includes its unguessable request token, not only PID.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    /// Native process ID; the owning group is verified separately for its role.
    pub pid: u32,
    /// Native process start time, combined with the exact command token.
    pub started_seconds: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Queued,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
}

impl Phase {
    pub(crate) fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StopReason {
    Cancelled,
    TimedOut,
    MemoryCap,
}

#[derive(Default)]
pub(crate) struct Completion<'reason> {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub reason: Option<&'reason str>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandOutcome {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// Effective build limits and memory enforcement selected for a running lease.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRecord {
    /// Effective Cargo jobs, when numeric and known.
    pub cargo_build_jobs: Option<u32>,
    /// True when an explicit Cargo jobs value was clamped to policy maximum.
    pub cargo_build_jobs_clamped: bool,
    /// Effective Rust test threads, when numeric and known.
    pub rust_test_threads: Option<u32>,
    /// True when an explicit Rust test thread value was clamped to policy maximum.
    pub rust_test_threads_clamped: bool,
    /// Effective `CARGO_INCREMENTAL` value, if set.
    pub cargo_incremental: Option<String>,
    /// Whether policy replaced the caller's `CARGO_INCREMENTAL` value.
    pub cargo_incremental_overwrote_caller: bool,
    /// Effective `CARGO_TARGET_DIR`, if set.
    pub cargo_target_dir: Option<PathBuf>,
    /// Whether policy replaced the caller's `CARGO_TARGET_DIR` value.
    pub cargo_target_dir_overwrote_caller: bool,
    /// Memory enforcement applied to the command.
    pub memory_enforcement: MemoryEnforcement,
}

/// Memory enforcement mechanism selected when the command starts.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEnforcement {
    /// No cap was configured for this lease.
    #[default]
    NotConfigured,
    /// A transient Linux systemd scope enforces the cap.
    SystemdScope,
    /// The watchdog samples process-group RSS and enforces the cap.
    RssWatchdog,
    /// The command was refused because no scope was available and policy requires one.
    Refused,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub id: RequestId,
    pub sequence: u64,
    pub owner: String,
    pub ticket: String,
    pub units: u32,
    pub priority: u32,
    /// Named priority class selected by the caller.
    #[serde(default = "default_request_class")]
    pub class: String,
    /// Memory limit reserved for this lease when policy configures one.
    #[serde(default)]
    pub memory_cap_bytes: Option<u64>,
    /// Caller-supplied build env values captured when the lease was queued.
    #[serde(default)]
    pub caller_cargo_build_jobs: Option<String>,
    /// Caller-supplied test-thread env value captured when the lease was queued.
    #[serde(default)]
    pub caller_rust_test_threads: Option<String>,
    /// Caller-supplied incremental setting captured when the lease was queued.
    #[serde(default)]
    pub caller_cargo_incremental: Option<String>,
    /// Caller-supplied Cargo target directory captured when the lease was queued.
    #[serde(default)]
    pub caller_cargo_target_dir: Option<String>,
    /// Effective per-lease settings, populated when its command starts.
    #[serde(default)]
    pub lease_record: Option<LeaseRecord>,
    pub enqueued_at_ms: u64,
    pub enqueued_clock_ms: u64,
    pub started_at_ms: Option<u64>,
    pub started_clock_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub ended_clock_ms: Option<u64>,
    pub heartbeat_at_ms: Option<u64>,
    pub worker: Option<ProcessIdentity>,
    pub worker_generation: u64,
    pub watchdog: Option<ProcessIdentity>,
    pub command_finished: Option<CommandOutcome>,
    pub phase: Phase,
    pub stop: Option<StopReason>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub reason: Option<String>,
    pub command: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct State {
    pub schema_version: u32,
    pub store_id: String,
    pub config_path: PathBuf,
    pub host: String,
    pub boot_time: u64,
    pub next_sequence: u64,
    pub next_event: u64,
    pub pruned_events: u64,
    #[serde(default, skip_serializing, deserialize_with = "ignore_legacy_metrics")]
    pub metrics: MetricsState,
    pub requests: Vec<Request>,
    pub events: Vec<Event>,
}

fn ignore_legacy_metrics<'de, D>(deserializer: D) -> std::result::Result<MetricsState, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let _ = serde_json::Value::deserialize(deserializer)?;
    Ok(MetricsState::default())
}

/// Stable transition evidence; command arguments and environment are excluded.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// Version of this event schema.
    pub schema_version: u32,
    /// Unique transition ID in this store.
    pub event_id: String,
    /// Stable request identity.
    pub request_id: RequestId,
    /// Configured host identity.
    pub host: String,
    /// Owning lead or lane.
    pub owner: String,
    /// Owning tracker issue.
    pub ticket: String,
    /// Transition UTC time.
    pub observed_at_ms: u64,
    /// Resulting phase.
    pub(crate) state: Phase,
    /// Claimed units.
    pub units: u32,
    /// Base priority.
    pub priority: u32,
    /// Named priority class captured at this transition.
    #[serde(default = "default_request_class")]
    pub class: String,
    /// Configured capacity at transition time.
    pub capacity_units: u32,
    /// Free units after transition, saturating at zero during shrink.
    pub free_units: u32,
    /// Wait duration at admission or current queue age.
    pub wait_ms: u64,
    /// Hold duration at terminal transition.
    pub hold_ms: Option<u64>,
    /// Termination/failure classification, if present.
    pub reason: Option<String>,
}

impl State {
    pub(crate) fn validate(&self, _config: &Config) -> Result<()> {
        if self.store_id.len() != 32
            || !self.store_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.requests.len() > crate::config::MAX_REQUESTS
            || self.next_event == 0
        {
            return Err(Error::new(
                ErrorCode::State,
                "invalid persisted store identity or bounds",
            ));
        }
        let mut sequences = BTreeSet::new();
        for request in &self.requests {
            if request.sequence == 0
                || request.sequence >= self.next_sequence
                || !sequences.insert(request.sequence)
                || request.id.0 != format!("{}:r{}", self.store_id, request.sequence)
                || request.units == 0
                || !matches!(
                    request.class.as_str(),
                    "must-fix" | "point" | "block" | "periodic"
                )
                || request.memory_cap_bytes.is_some_and(|cap| cap == 0)
                || request
                    .caller_cargo_build_jobs
                    .as_ref()
                    .is_some_and(|value| value.len() > 64)
                || request
                    .caller_rust_test_threads
                    .as_ref()
                    .is_some_and(|value| value.len() > 64)
                || request
                    .caller_cargo_incremental
                    .as_ref()
                    .is_some_and(|value| value.len() > 64)
                || request
                    .caller_cargo_target_dir
                    .as_ref()
                    .is_some_and(|value| value.len() > 32_768)
                || request.command.is_empty()
                || request.command.len() > 256
                || request
                    .command
                    .iter()
                    .any(|argument| argument.len() > 32_768)
                || request.owner.is_empty()
                || request.owner.len() > 256
                || request.ticket.is_empty()
                || request.ticket.len() > 128
                || (request.worker.is_some() && request.worker_generation == 0)
                || (request.phase == Phase::Queued
                    && (request.watchdog.is_some() || request.command_finished.is_some()))
                || (request.phase == Phase::Running
                    && (request.worker.is_none()
                        || request.started_clock_ms.is_none()
                        || request.started_at_ms.is_none()))
                || (request.phase.terminal()
                    && (request.ended_at_ms.is_none() || request.ended_clock_ms.is_none()))
            {
                return Err(Error::new(ErrorCode::State, "invalid persisted request"));
            }
        }
        Ok(())
    }

    pub(crate) fn find(&self, id: &RequestId) -> Result<&Request> {
        self.requests
            .iter()
            .find(|request| request.id == *id)
            .ok_or_else(|| Error::new(ErrorCode::Request, "unknown request"))
    }

    pub(crate) fn find_mut(&mut self, id: &RequestId) -> Result<&mut Request> {
        self.requests
            .iter_mut()
            .find(|request| request.id == *id)
            .ok_or_else(|| Error::new(ErrorCode::Request, "unknown request"))
    }

    pub(crate) fn used(&self) -> u32 {
        self.requests
            .iter()
            .filter(|request| request.phase == Phase::Running)
            .fold(0_u32, |sum, request| sum.saturating_add(request.units))
    }

    fn memory_used(&self, config: &Config) -> u64 {
        self.requests
            .iter()
            .filter(|request| request.phase == Phase::Running)
            .fold(0_u64, |sum, request| {
                sum.saturating_add(
                    request
                        .memory_cap_bytes
                        .or(config.memory_cap_bytes)
                        .unwrap_or(0),
                )
            })
    }

    pub(crate) fn event(
        &mut self,
        id: &RequestId,
        config: &Config,
        now: u64,
        clock: u64,
    ) -> Result<()> {
        let request = self.find(id)?;
        let monotonic_wait = request
            .started_clock_ms
            .unwrap_or(clock)
            .saturating_sub(request.enqueued_clock_ms);
        let event = Event {
            schema_version: 1,
            event_id: format!("{}:e{}", self.store_id, self.next_event),
            request_id: id.clone(),
            host: self.host.clone(),
            owner: request.owner.clone(),
            ticket: request.ticket.clone(),
            observed_at_ms: now,
            state: request.phase,
            units: request.units,
            priority: request.priority,
            class: request.class.clone(),
            capacity_units: config.capacity_units,
            free_units: config.capacity_units.saturating_sub(self.used()),
            wait_ms: monotonic_wait,
            hold_ms: request
                .started_clock_ms
                .map(|started| clock.saturating_sub(started)),
            reason: request.reason.clone(),
        };
        self.next_event = self
            .next_event
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "event sequence exhausted"))?;
        self.events.push(event);
        Ok(())
    }

    pub(crate) fn next<'a>(&'a self, config: &Config, clock: u64) -> Option<&'a Request> {
        let feasible = || {
            self.requests.iter().filter(|request| {
                request.phase == Phase::Queued
                    && request.units <= config.capacity_units
                    && request.stop.is_none()
            })
        };
        let reservation_ms = (u64::from(config.max_priority()) + 1).saturating_mul(config.aging_ms);
        if let Some(reserved) = feasible()
            .filter(|request| clock.saturating_sub(request.enqueued_clock_ms) >= reservation_ms)
            .min_by_key(|request| request.sequence)
        {
            return Some(reserved);
        }
        feasible().max_by(|left, right| {
            let priority = |request: &Request| {
                u64::from(request.priority).saturating_add(
                    clock.saturating_sub(request.enqueued_clock_ms) / config.aging_ms,
                )
            };
            priority(left)
                .cmp(&priority(right))
                .then_with(|| right.sequence.cmp(&left.sequence))
        })
    }

    pub(crate) fn admission_reason(
        &self,
        id: &RequestId,
        config: &Config,
        sample: Option<&HostSample>,
        now: u64,
        clock: u64,
    ) -> Result<Option<&'static str>> {
        let request = self.find(id)?;
        if request.units > config.capacity_units {
            return Ok(Some("capacity_shrunk"));
        }
        let Some(next) = self.next(config, clock) else {
            return Ok(Some("no_eligible_request"));
        };
        if next.id != *id {
            return Ok(Some("fifo_or_priority_reservation"));
        }
        let used = self.used();
        if u64::from(used) + u64::from(request.units) > u64::from(config.capacity_units) {
            return Ok(Some("capacity_occupied"));
        }
        let Some(sample) = sample else {
            return Ok(Some("host_sample_unavailable"));
        };
        if let Some(reason) = sample.block_reason(config, now, used, request.units) {
            return Ok(Some(reason));
        }
        if config.memory_admission == MemoryAdmissionMode::LeaseAccounted {
            let budget = config.memory_budget_bytes.unwrap_or(0);
            let cap = request
                .memory_cap_bytes
                .or(config.memory_cap_bytes)
                .unwrap_or(0);
            if self.memory_used(config).saturating_add(cap) > budget {
                return Ok(Some("memory_budget"));
            }
        }
        Ok(None)
    }

    pub(crate) fn finish(
        &mut self,
        id: &RequestId,
        completion: Completion<'_>,
        config: &Config,
        now: u64,
        clock: u64,
    ) -> Result<()> {
        if self.find(id)?.phase.terminal() {
            return Ok(());
        }
        let Completion {
            code,
            signal,
            reason,
        } = completion;
        let request = self.find_mut(id)?;
        request.phase = match request.stop {
            Some(StopReason::Cancelled) => Phase::Cancelled,
            Some(StopReason::TimedOut) => Phase::TimedOut,
            Some(StopReason::MemoryCap) => Phase::Failed,
            None if code == Some(0) && signal.is_none() && reason.is_none() => Phase::Succeeded,
            None => Phase::Failed,
        };
        request.exit_code = code;
        request.signal = signal;
        request.reason = request
            .stop
            .map(|stop| match stop {
                StopReason::Cancelled => "cancelled".to_owned(),
                StopReason::TimedOut => "hold_timeout".to_owned(),
                StopReason::MemoryCap => "rss_cap".to_owned(),
            })
            .or_else(|| reason.map(str::to_owned))
            .or_else(|| {
                if signal.is_some() {
                    Some("command_signaled".to_owned())
                } else if code.is_some_and(|code| code != 0) {
                    Some("command_exit".to_owned())
                } else {
                    None
                }
            });
        request.ended_at_ms = Some(now);
        request.ended_clock_ms = Some(clock);
        self.event(id, config, now, clock)
    }
}

#[cfg(test)]
mod tests;
