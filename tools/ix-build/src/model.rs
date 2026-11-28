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

/// Durable launch authorization, written before a worker process is created.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkerClaim {
    pub protocol: u32,
    pub store_id: String,
    pub request_id: RequestId,
    pub generation: u64,
    pub launcher: ProcessIdentity,
    pub pid_namespace: Option<String>,
    pub worker: Option<ProcessIdentity>,
}

/// A wakeup hint. The state snapshot and worker fence remain authoritative.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestNotice {
    pub store_id: String,
    pub request_id: RequestId,
    pub generation: u64,
    pub phase: Phase,
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

pub(crate) fn hold_timeout_reason(hold_ms: u64) -> String {
    format!(
        "hold_timeout: command exceeded policy hold_ms={hold_ms} ms; change the limit in policy hold_ms"
    )
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

/// Per-request wrapper decision, recorded alongside measured lease limits.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheWrapperStatus {
    /// Old records contain no wrapper selection evidence.
    #[default]
    Unrecorded,
    /// A runnable kache binary was selected.
    Enabled,
    /// Host policy disabled the wrapper.
    Disabled,
    /// No executable was discovered.
    Missing,
    /// The selected executable failed its bounded version check.
    Unhealthy,
    /// Policy changed after preparation; this request runs without a wrapper.
    PolicyChanged,
}

/// Effective build limits and memory enforcement selected for a running lease.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRecord {
    /// Effective kache executable, absent when caching was skipped.
    #[serde(default, skip_serializing_if = "ix_build::wire::persistence::hide")]
    pub cache_wrapper: Option<PathBuf>,
    /// Why the per-request wrapper was selected or skipped.
    #[serde(default, skip_serializing_if = "ix_build::wire::persistence::hide")]
    pub cache_wrapper_status: CacheWrapperStatus,
    /// Exact systemd scope owned by this lease, excluding nested caller scopes.
    #[serde(default, skip_serializing_if = "ix_build::wire::persistence::hide")]
    pub memory_scope: Option<String>,
    /// Observed kernel OOM-kill counter for this command's memory scope.
    #[serde(default)]
    pub oom_kill_count: u64,
    /// The owned systemd unit reported Result=oom-kill; not a counter sample.
    #[serde(default, skip_serializing_if = "ix_build::wire::persistence::hide")]
    pub scope_oom_killed: bool,
    /// Identity of the launched command; memory accounting starts here.
    #[serde(default)]
    pub command: Option<ProcessIdentity>,
    /// Measured memory that triggered enforcement, including its metric.
    #[serde(default)]
    pub memory_sample: Option<MemorySample>,
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

/// Memory measurements name their accounting basis as well as bytes.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySample {
    /// Per-process contributions for a watchdog sample.
    #[serde(default)]
    pub contributors: Vec<ProcessMemory>,
    /// Measured bytes, before the command was terminated.
    pub bytes: u64,
    /// Accounting metric used by enforcement.
    pub metric: MemoryMetric,
}

/// One process's contribution to a measured command tree.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessMemory {
    /// PID in the lease's namespace.
    pub pid: u32,
    /// Bytes attributed by the named sample metric.
    pub bytes: u64,
}

/// Metrics used by the memory enforcement backends.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMetric {
    /// Linux proportional anonymous memory, excluding file/shmem mappings.
    PssAnon,
    /// macOS resident set size.
    Rss,
    /// Kernel cgroup peak memory (includes charged file cache).
    CgroupMemoryPeak,
}

impl MemoryMetric {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PssAnon => "pss_anon",
            Self::Rss => "rss",
            Self::CgroupMemoryPeak => "cgroup_memory_peak",
        }
    }
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
    /// The watchdog samples the command tree (Linux Pss_Anon, macOS RSS).
    RssWatchdog,
    /// The command was refused because no scope was available and policy requires one.
    Refused,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Request {
    /// Original caller identity, independent of later scheduler launches.
    #[serde(default)]
    pub client: Option<ProcessIdentity>,
    #[serde(default)]
    pub client_pid_namespace: Option<String>,
    #[serde(default)]
    pub telemetry: crate::telemetry::EventTelemetry,
    #[serde(default)]
    pub supervision_detail: Option<String>,
    #[serde(default, flatten)]
    pub trace: ix_build::wire::RequestTrace,
    #[serde(default)]
    pub command_started_clock_ms: Option<u64>,
    #[serde(default)]
    pub blocked_at_clock_ms: Option<u64>,
    #[serde(default)]
    pub active_blocked_reasons: Vec<String>,
    #[serde(default)]
    pub measured_memory_bytes: Option<u64>,
    /// Linux PID namespace in which worker/watchdog IDs are meaningful.
    #[serde(default)]
    pub pid_namespace: Option<String>,
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
    /// Admission reservation, independent of the enforced hard cap.
    #[serde(default)]
    pub memory_reservation_bytes: Option<u64>,
    /// Policy source selected with the recorded hard cap, before policy edits.
    #[serde(default)]
    pub memory_limit_key: Option<String>,
    /// Cargo job count paired with the reserved memory lease.
    #[serde(default)]
    pub memory_cargo_jobs: Option<u32>,
    /// Rust test thread count paired with the reserved memory lease.
    #[serde(default)]
    pub memory_rust_test_threads: Option<u32>,
    /// Maximum parallel workers charged to this memory lease.
    #[serde(default)]
    pub memory_parallelism: Option<u32>,
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
    #[serde(serialize_with = "ix_build::wire::persistence::schema")]
    pub schema_version: u32,
    #[serde(
        default = "legacy_reader_version",
        skip_serializing_if = "ix_build::wire::persistence::hide"
    )]
    pub min_reader_version: u32,
    pub store_id: String,
    pub config_path: PathBuf,
    pub host: String,
    pub boot_time: u64,
    #[serde(default)]
    pub boot_id: Option<String>,
    pub next_sequence: u64,
    pub next_event: u64,
    pub pruned_events: u64,
    #[serde(default, skip_serializing, deserialize_with = "ignore_legacy_metrics")]
    pub metrics: MetricsState,
    pub requests: Vec<Request>,
    pub events: Vec<Event>,
}

fn legacy_reader_version() -> u32 {
    1
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
pub struct Event {
    /// Frozen build measurements and bounded identity inputs.
    #[serde(default)]
    pub(crate) telemetry: crate::telemetry::EventTelemetry,
    /// Repairs a previously published outcome without counting a new attempt.
    #[serde(default)]
    pub correction: bool,
    /// Queue/diagnostic fields shared with status consumers.
    #[serde(default, flatten)]
    pub trace: ix_build::wire::RequestTrace,
    /// transition, blocked, command_started, or refused.
    #[serde(default = "transition_kind")]
    pub kind: String,
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

fn transition_kind() -> String {
    "transition".to_owned()
}

impl Request {
    pub(crate) fn notice(&self, store_id: &str) -> RequestNotice {
        RequestNotice {
            store_id: store_id.to_owned(),
            request_id: self.id.clone(),
            generation: self.worker_generation,
            phase: self.phase,
        }
    }
    pub(crate) fn reservation_bytes(&self) -> u64 {
        self.memory_reservation_bytes
            .or(self.memory_cap_bytes)
            .unwrap_or(0)
    }
    pub(crate) fn trace_snapshot(&self, clock: u64) -> ix_build::wire::RequestTrace {
        let mut trace = self.trace.clone();
        let end = self.ended_clock_ms.unwrap_or(clock);
        trace.queued_at_ms = Some(self.enqueued_at_ms);
        trace.admitted_at_ms = self.started_at_ms;
        trace.finished_at_ms = self.ended_at_ms;
        trace.queued_seconds = self
            .started_clock_ms
            .unwrap_or(end)
            .saturating_sub(self.enqueued_clock_ms) as f64
            / 1000.0;
        trace.admitted_seconds = self.started_clock_ms.map_or(0.0, |start| {
            self.command_started_clock_ms
                .unwrap_or(end)
                .saturating_sub(start) as f64
                / 1000.0
        });
        trace.running_seconds = self
            .command_started_clock_ms
            .map_or(0.0, |start| end.saturating_sub(start) as f64 / 1000.0);
        trace.lifecycle_state = if self.phase.terminal() && self.trace.lifecycle_state == "refused"
        {
            "refused"
        } else {
            match self.phase {
                Phase::Queued => "queued",
                Phase::Running if self.command_started_clock_ms.is_none() => "admitted",
                Phase::Running => "running",
                Phase::Succeeded => "succeeded",
                Phase::Failed => "failed",
                Phase::Cancelled => "cancelled",
                Phase::TimedOut => "timed_out",
            }
        }
        .to_owned();
        trace.position = if self.phase == Phase::Queued {
            trace.position
        } else {
            None
        };
        trace.granted_jobs = self
            .lease_record
            .as_ref()
            .and_then(|lease| lease.cargo_build_jobs);
        trace.granted_memory_bytes = self.started_at_ms.and(self.memory_cap_bytes);
        trace.granted_memory_reservation_bytes = self
            .started_at_ms
            .and(self.memory_reservation_bytes.or(self.memory_cap_bytes));
        if let Some(last) = self.blocked_at_clock_ms {
            for reason in &mut trace.blocked_reasons {
                if self.active_blocked_reasons.contains(&reason.reason) {
                    reason.seconds += end.saturating_sub(last) as f64 / 1000.0;
                }
            }
        }
        trace
    }

    pub(crate) fn observe_blocks(
        &mut self,
        reasons: Vec<ix_build::wire::BlockObservation>,
        clock: u64,
    ) -> bool {
        let changed = self.active_blocked_reasons
            != reasons.iter().map(|r| r.reason.clone()).collect::<Vec<_>>();
        self.trace.blocked_reasons = self.trace_snapshot(clock).blocked_reasons;
        self.active_blocked_reasons.clear();
        for observation in reasons {
            self.active_blocked_reasons.push(observation.reason.clone());
            if let Some(old) = self
                .trace
                .blocked_reasons
                .iter_mut()
                .find(|r| r.reason == observation.reason)
            {
                old.value = observation.value;
                old.limit = observation.limit;
            } else {
                self.trace.blocked_reasons.push(observation);
            }
        }
        self.trace
            .blocked_reasons
            .sort_by(|a, b| a.reason.cmp(&b.reason));
        self.blocked_at_clock_ms = Some(clock);
        changed
    }
}

impl State {
    pub(crate) fn system_event(&mut self, kind: &str, config: &Config, now: u64) -> Result<()> {
        let event = Event {
            telemetry: Default::default(),
            correction: false,
            trace: Default::default(),
            kind: kind.to_owned(),
            schema_version: 1,
            event_id: format!("{}:e{}", self.store_id, self.next_event),
            request_id: RequestId(format!("{}:system", self.store_id)),
            host: self.host.clone(),
            owner: String::new(),
            ticket: String::new(),
            observed_at_ms: now,
            state: Phase::Queued,
            units: 0,
            priority: 0,
            class: "periodic".to_owned(),
            capacity_units: config.capacity_units,
            free_units: config.capacity_units.saturating_sub(self.used()),
            wait_ms: 0,
            hold_ms: None,
            reason: None,
        };
        self.next_event = self
            .next_event
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "event sequence exhausted"))?;
        self.events.push(event);
        Ok(())
    }
    pub(crate) fn rebooted(&self, boot_id: Option<&str>, boot_time: u64) -> bool {
        match (self.boot_id.as_deref(), boot_id) {
            (Some(previous), Some(current)) => previous != current,
            (None, Some(_)) => false,
            (_, None) => {
                self.boot_time != 0 && boot_time != 0 && self.boot_time.abs_diff(boot_time) > 120
            }
        }
    }
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

    pub(crate) fn memory_used(&self, _config: &Config) -> u64 {
        self.requests
            .iter()
            .filter(|request| request.phase == Phase::Running)
            .fold(0_u64, |sum, request| {
                sum.saturating_add(request.reservation_bytes())
            })
    }

    pub(crate) fn event(
        &mut self,
        id: &RequestId,
        config: &Config,
        now: u64,
        clock: u64,
    ) -> Result<()> {
        let snapshot = self.find(id)?.trace_snapshot(clock);
        self.find_mut(id)?.trace = snapshot;
        if self.find(id)?.phase == Phase::Queued {
            let position = self.queue_position(id, config, clock);
            self.find_mut(id)?.trace.position = position;
        }
        // The stored trace now includes open blocking time through this clock.
        if self.find(id)?.blocked_at_clock_ms.is_some() {
            self.find_mut(id)?.blocked_at_clock_ms = Some(clock);
        }
        let request = self.find(id)?;
        let monotonic_wait = request
            .started_clock_ms
            .unwrap_or(clock)
            .saturating_sub(request.enqueued_clock_ms);
        let event = Event {
            telemetry: crate::telemetry::EventTelemetry::for_request(request, config),
            correction: false,
            trace: request.trace_snapshot(clock),
            kind: transition_kind(),
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
        // Urgent work always wins over lower classes, even after aging.
        if let Some(urgent) = feasible()
            .filter(|r| r.class == "must-fix")
            .min_by_key(|r| r.sequence)
        {
            return Some(urgent);
        }
        let reservation_ms = (u64::from(config.max_priority()) + 1).saturating_mul(config.aging_ms);
        if let Some(reserved) = feasible()
            .filter(|request| clock.saturating_sub(request.enqueued_clock_ms) >= reservation_ms)
            .min_by_key(|request| request.sequence)
        {
            return Some(reserved);
        }
        feasible().max_by(|left, right| {
            let priority = |request: &Request| {
                u64::from(request.priority)
                    .saturating_add(
                        clock.saturating_sub(request.enqueued_clock_ms) / config.aging_ms,
                    )
                    .min(u64::from(config.max_priority()))
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
            // Backfill only memory-blocked heads before their bounded reservation.
            // Capacity remains charged to every admitted lease; never overcommit.
            let reservation_ms =
                (u64::from(config.max_priority()) + 1).saturating_mul(config.aging_ms);
            let free = config
                .memory_budget_bytes
                .unwrap_or(0)
                .saturating_sub(self.memory_used(config));
            let backfill = config.memory_admission == MemoryAdmissionMode::LeaseAccounted
                && next.reservation_bytes() > free
                && clock.saturating_sub(next.enqueued_clock_ms) < reservation_ms
                && request.reservation_bytes() <= free;
            if !backfill {
                return Ok(Some("fifo_or_priority_reservation"));
            }
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
            let cap = request.reservation_bytes();
            if self.memory_used(config).saturating_add(cap) > budget {
                return Ok(Some("memory_budget"));
            }
        }
        Ok(None)
    }

    pub(crate) fn queue_position(
        &self,
        id: &RequestId,
        config: &Config,
        clock: u64,
    ) -> Option<u32> {
        self.queue_positions(config, clock).get(&id.0).copied()
    }

    pub(crate) fn queue_positions(
        &self,
        config: &Config,
        clock: u64,
    ) -> std::collections::BTreeMap<String, u32> {
        let mut queued: Vec<_> = self
            .requests
            .iter()
            .filter(|r| r.phase == Phase::Queued && r.stop.is_none())
            .collect();
        let score = |r: &Request| {
            u64::from(r.priority)
                .saturating_add(clock.saturating_sub(r.enqueued_clock_ms) / config.aging_ms)
                .min(u64::from(config.max_priority()))
        };
        queued.sort_by(|a, b| {
            (b.class == "must-fix")
                .cmp(&(a.class == "must-fix"))
                .then_with(|| score(b).cmp(&score(a)))
                .then_with(|| a.sequence.cmp(&b.sequence))
        });
        if let Some(head) = self.next(config, clock)
            && let Some(index) = queued.iter().position(|r| r.id == head.id)
        {
            let first = queued.remove(index);
            queued.insert(0, first);
        }
        queued
            .iter()
            .enumerate()
            .filter_map(|(index, r)| {
                u32::try_from(index + 1)
                    .ok()
                    .map(|position| (r.id.0.clone(), position))
            })
            .collect()
    }

    pub(crate) fn block_observations(
        &self,
        id: &RequestId,
        config: &Config,
        sample: Option<&HostSample>,
        now: u64,
        clock: u64,
    ) -> Result<Vec<ix_build::wire::BlockObservation>> {
        let request = self.find(id)?;
        let mut reasons = Vec::new();
        let mut add = |reason: &str, value: f64, limit: f64| {
            reasons.push(ix_build::wire::BlockObservation {
                reason: reason.to_owned(),
                value,
                limit,
                seconds: 0.0,
            })
        };
        let units = u64::from(self.used()) + u64::from(request.units);
        if units > u64::from(config.capacity_units) {
            add("units", units as f64, f64::from(config.capacity_units));
        }
        if config.memory_admission == MemoryAdmissionMode::LeaseAccounted {
            let value = self
                .memory_used(config)
                .saturating_add(request.reservation_bytes());
            let limit = config.memory_budget_bytes.unwrap_or(0);
            if value > limit {
                add("memory_budget", value as f64, limit as f64);
            }
        }
        if let Some(sample) = sample.filter(|sample| {
            sample.block_reason(config, now, self.used(), request.units)
                != Some("host_sample_invalid_or_stale")
        }) {
            if sample.load_one >= config.maximum_load {
                add("load", sample.load_one, config.maximum_load);
            }
            if sample.disk_available_bytes < config.disk_reserve_bytes {
                add(
                    "disk",
                    sample.disk_available_bytes as f64,
                    config.disk_reserve_bytes as f64,
                );
            }
            if config.memory_admission == MemoryAdmissionMode::LiveAvailable {
                let limit = config
                    .memory_reserve_bytes
                    .saturating_add(config.memory_per_unit_bytes.saturating_mul(units));
                if sample.memory_available_bytes < limit {
                    add(
                        "memory_reserve",
                        sample.memory_available_bytes as f64,
                        limit as f64,
                    );
                }
            }
        } else {
            add("telemetry_unavailable", 0.0, 1.0);
        }
        if self.admission_reason(id, config, sample, now, clock)?
            == Some("fifo_or_priority_reservation")
        {
            add(
                "priority_reservation",
                f64::from(self.queue_position(id, config, clock).unwrap_or(1)),
                1.0,
            );
        }
        reasons.sort_by(|a, b| a.reason.cmp(&b.reason));
        Ok(reasons)
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
        let reason = reason.or(
            if request.reason.as_deref() == Some("supervision_lost")
                && request.supervision_detail.is_some()
            {
                Some("supervision_lost")
            } else {
                None
            },
        );
        request.observe_blocks(Vec::new(), clock);
        request.phase = match request.stop {
            Some(StopReason::Cancelled) => Phase::Cancelled,
            Some(StopReason::TimedOut) => Phase::TimedOut,
            Some(StopReason::MemoryCap) => Phase::Failed,
            None if code == Some(0)
                && signal.is_none()
                && (reason.is_none()
                    || matches!(
                        reason,
                        Some("supervisor_lost" | "supervision_lost" | "supervision_degraded")
                    )) =>
            {
                Phase::Succeeded
            }
            None => Phase::Failed,
        };
        request.exit_code = code;
        request.signal = signal;
        request.reason = request
            .stop
            .map(|stop| match stop {
                StopReason::Cancelled => "cancelled".to_owned(),
                StopReason::TimedOut => hold_timeout_reason(config.hold_ms),
                StopReason::MemoryCap => "rss_cap".to_owned(),
            })
            .or_else(|| {
                if request.phase == Phase::Succeeded && reason.is_some() {
                    Some("supervision_degraded".to_owned())
                } else {
                    reason.map(str::to_owned)
                }
            })
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
        request.trace.diagnostic = crate::diagnostics::for_request(request, config, clock);
        self.event(id, config, now, clock)
    }
}

#[cfg(test)]
mod tests;

/// Durable acknowledgement of a successfully published telemetry ledger.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportCheckpoint {
    pub store_id: String,
    pub sequence: u64,
}
