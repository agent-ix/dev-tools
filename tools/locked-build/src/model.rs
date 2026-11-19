// SPDX-License-Identifier: MIT
//! Persistent admission state and deterministic priority/FIFO decisions.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{Config, HostSample};
use crate::error::{Error, ErrorCode, Result};

/// Globally random store identity plus monotonically increasing request number.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct RequestId(pub String);

/// A supervisor identity includes its unguessable request token, not only PID.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    /// Process ID and process-group ID of the stable supervisor.
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
}

#[derive(Default)]
pub(crate) struct Completion<'reason> {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub reason: Option<&'reason str>,
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
    pub enqueued_at_ms: u64,
    pub enqueued_clock_ms: u64,
    pub started_at_ms: Option<u64>,
    pub started_clock_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub ended_clock_ms: Option<u64>,
    pub heartbeat_at_ms: Option<u64>,
    pub worker: Option<ProcessIdentity>,
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
    pub requests: Vec<Request>,
    pub events: Vec<Event>,
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
    pub state: String,
    /// Claimed units.
    pub units: u32,
    /// Base priority.
    pub priority: u32,
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
    pub(crate) fn validate(&self, config: &Config) -> Result<()> {
        if self.store_id.len() != 32
            || !self.store_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.requests.len() > config.maximum_requests
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

    pub(crate) fn event(
        &mut self,
        id: &RequestId,
        config: &Config,
        now: u64,
        clock: u64,
    ) -> Result<()> {
        let request = self.find(id)?;
        let event = Event {
            schema_version: 1,
            event_id: format!("{}:e{}", self.store_id, self.next_event),
            request_id: id.clone(),
            host: self.host.clone(),
            owner: request.owner.clone(),
            ticket: request.ticket.clone(),
            observed_at_ms: now,
            state: serde_json::to_value(request.phase)?
                .as_str()
                .ok_or_else(|| Error::new(ErrorCode::State, "invalid phase serialization"))?
                .to_owned(),
            units: request.units,
            priority: request.priority,
            capacity_units: config.capacity_units,
            free_units: config.capacity_units.saturating_sub(self.used()),
            wait_ms: request
                .started_clock_ms
                .unwrap_or(clock)
                .saturating_sub(request.enqueued_clock_ms),
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
        let reservation_ms =
            (u64::from(config.maximum_priority) + 1).saturating_mul(config.aging_ms);
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
        Ok(sample.block_reason(config, now, used, request.units))
    }

    pub(crate) fn finish(
        &mut self,
        id: &RequestId,
        completion: Completion<'_>,
        config: &Config,
        now: u64,
        clock: u64,
    ) -> Result<()> {
        let Completion {
            code,
            signal,
            reason,
        } = completion;
        let request = self.find_mut(id)?;
        request.phase = match request.stop {
            Some(StopReason::Cancelled) => Phase::Cancelled,
            Some(StopReason::TimedOut) => Phase::TimedOut,
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
