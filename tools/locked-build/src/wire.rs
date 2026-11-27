// SPDX-License-Identifier: MIT
//! Shared error, queue and metrics records for producers and monitoring readers.

use serde::{Deserialize, Serialize};

/// Stable abnormal-outcome codes. Spellings are part of the monitoring contract.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub enum Code {
    /// Watchdog memory limit.
    #[serde(rename = "LB_RSS_CAP")] RssCap,
    /// Kernel cgroup OOM kill.
    #[serde(rename = "LB_OOM_KILLED")] OomKilled,
    /// Maximum lease duration.
    #[serde(rename = "LB_HOLD_TIMEOUT")] HoldTimeout,
    /// Supervision was lost or degraded.
    #[serde(rename = "LB_SUPERVISION_LOST")] SupervisionLost,
    /// Requested memory exceeds the host budget.
    #[serde(rename = "LB_REFUSED_MEMORY_BUDGET")] RefusedMemoryBudget,
    /// Requested memory exceeds the class cap.
    #[serde(rename = "LB_REFUSED_CLASS_CAP")] RefusedClassCap,
    /// Queue or unit limit.
    #[serde(rename = "LB_REFUSED_QUEUE")] RefusedQueue,
    /// Required memory enforcement is unavailable.
    #[serde(rename = "LB_REFUSED_CAP_UNAVAILABLE")] RefusedCapUnavailable,
    /// Caller cancellation.
    #[serde(rename = "LB_CANCELLED")] Cancelled,
    /// A signal was not initiated by locked-build.
    #[serde(rename = "LB_EXTERNAL_SIGNAL")] ExternalSignal,
    /// Nonzero command exit.
    #[serde(rename = "LB_COMMAND_EXIT")] CommandExit,
    /// Invalid request.
    #[serde(rename = "LB_INVALID_REQUEST")] InvalidRequest,
    /// State or configuration is unavailable.
    #[serde(rename = "LB_STATE_UNAVAILABLE")] StateUnavailable,
}

impl Code {
    /// Every supported code, for bounded metrics and exhaustive validation.
    pub const ALL: [Self; 13] = [Self::RssCap, Self::OomKilled, Self::HoldTimeout, Self::SupervisionLost, Self::RefusedMemoryBudget, Self::RefusedClassCap, Self::RefusedQueue, Self::RefusedCapUnavailable, Self::Cancelled, Self::ExternalSignal, Self::CommandExit, Self::InvalidRequest, Self::StateUnavailable];
    /// Stable wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RssCap => "LB_RSS_CAP", Self::OomKilled => "LB_OOM_KILLED",
            Self::HoldTimeout => "LB_HOLD_TIMEOUT", Self::SupervisionLost => "LB_SUPERVISION_LOST",
            Self::RefusedMemoryBudget => "LB_REFUSED_MEMORY_BUDGET", Self::RefusedClassCap => "LB_REFUSED_CLASS_CAP",
            Self::RefusedQueue => "LB_REFUSED_QUEUE", Self::RefusedCapUnavailable => "LB_REFUSED_CAP_UNAVAILABLE",
            Self::Cancelled => "LB_CANCELLED", Self::ExternalSignal => "LB_EXTERNAL_SIGNAL",
            Self::CommandExit => "LB_COMMAND_EXIT", Self::InvalidRequest => "LB_INVALID_REQUEST",
            Self::StateUnavailable => "LB_STATE_UNAVAILABLE",
        }
    }
}

/// Exact integer values and finite telemetry numbers, plus named policy rules.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Value {
    /// Integer measurement or limit.
    Integer(u64),
    /// Fractional telemetry.
    Number(f64),
    /// Boolean built-in policy rule.
    Boolean(bool),
    /// Non-numeric policy value or unavailable measurement.
    Text(String),
}

impl std::fmt::Display for Value {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Integer(value) => write!(out, "{value}"), Self::Number(value) => write!(out, "{value}"), Self::Boolean(value) => write!(out, "{value}"), Self::Text(value) => out.write_str(value) }
    }
}
impl From<u64> for Value { fn from(value: u64) -> Self { Self::Integer(value) } }
impl From<u32> for Value { fn from(value: u32) -> Self { Self::Integer(u64::from(value)) } }
impl From<bool> for Value { fn from(value: bool) -> Self { Self::Boolean(value) } }
impl From<&str> for Value { fn from(value: &str) -> Self { Self::Text(value.to_owned()) } }
impl From<String> for Value { fn from(value: String) -> Self { Self::Text(value) } }

/// Structured fields and the exact caller/ledger message for an abnormal outcome.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Diagnostic {
    /// Stable code, absent on ordinary success or before completion.
    pub code: Option<Code>,
    /// Exact single stderr line, without its terminating newline.
    pub message: Option<String>,
    /// Policy key or named built-in rule.
    pub limit_key: Option<String>,
    /// Configured/derived limit.
    pub limit_value: Option<Value>,
    /// Observed value, or the string unavailable.
    pub measured_value: Option<Value>,
    /// Measurement name, unit, or observed failure condition.
    pub measured_what: Option<String>,
    /// Process/group/scope or not-started command.
    pub killed: Option<String>,
}

impl Diagnostic {
    /// Construct structured metadata and its one canonical, single-line message.
    pub fn failure(code: Code, request: &str, key: &str, limit: Value, measured: Value, what: &str, killed: &str) -> Self {
        let what = what.split_whitespace().collect::<Vec<_>>().join(" ");
        let killed = killed.split_whitespace().collect::<Vec<_>>().join("_");
        let message = format!("locked-build: {} request={request} limit={key}={limit} measured={measured} ({what}) killed={killed}", code.as_str());
        Self { code: Some(code), message: Some(message), limit_key: Some(key.to_owned()), limit_value: Some(limit), measured_value: Some(measured), measured_what: Some(what), killed: Some(killed) }
    }
}

/// One cumulative queued blocking reason; simultaneous reasons may overlap.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BlockObservation {
    /// Closed reason vocabulary from locked-build-wire.md.
    pub reason: String,
    /// Last observed numeric value.
    pub value: f64,
    /// Last configured numeric limit.
    pub limit: f64,
    /// Cumulative monotonic blocked seconds.
    pub seconds: f64,
}

/// The shared flat diagnostic and queue fields attached to requests and events.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RequestTrace {
    /// Shared diagnostic fields, serialized flat.
    #[serde(flatten)] pub diagnostic: Diagnostic,
    /// Precise queued/admitted/running/terminal lifecycle state.
    pub lifecycle_state: String,
    /// Queue depth including this request on enqueue, zero for a refusal.
    pub queue_depth_at_enqueue: u32,
    /// One-based queue rank, absent outside queued state.
    pub position: Option<u32>,
    /// UTC queue entry time.
    pub queued_at_ms: Option<u64>,
    /// UTC admission time.
    pub admitted_at_ms: Option<u64>,
    /// UTC command start time.
    pub running_at_ms: Option<u64>,
    /// UTC completion/refusal time.
    pub finished_at_ms: Option<u64>,
    /// Monotonic queued duration, seconds.
    pub queued_seconds: f64,
    /// Monotonic admitted-before-start duration, seconds.
    pub admitted_seconds: f64,
    /// Monotonic command-running duration, seconds.
    pub running_seconds: f64,
    /// Cumulative blocking observations.
    pub blocked_reasons: Vec<BlockObservation>,
    /// Caller-requested numeric jobs.
    pub requested_jobs: Option<u32>,
    /// Actual numeric jobs at command start.
    pub granted_jobs: Option<u32>,
    /// Requested admission memory estimate.
    pub requested_memory_bytes: Option<u64>,
    /// Reserved/admitted memory cap.
    pub granted_memory_bytes: Option<u64>,
}

/// One counter series in status JSON.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OutcomeCounter {
    /// Host label.
    pub host: String,
    /// Class label.
    pub class: String,
    /// Closed terminal/refusal outcome.
    pub outcome: String,
    /// Durable counter value.
    pub value: u64,
}
/// One code counter series in status JSON.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CodeCounter {
    /// Host label.
    pub host: String,
    /// Class label.
    pub class: String,
    /// Stable termination/refusal code.
    pub code: Code,
    /// Durable counter value.
    pub value: u64,
}
/// One class/reason blocked-duration series.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BlockedCounter {
    /// Host label.
    pub host: String,
    /// Class label.
    pub class: String,
    /// Blocking reason label.
    pub reason: String,
    /// Cumulative blocked seconds.
    pub value: f64,
}
/// Histogram upper bound, including the Prometheus +Inf spelling.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum BucketBound {
    /// Finite seconds.
    Seconds(u64),
    /// The string +Inf.
    Infinity(String),
}
/// One cumulative histogram bucket.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HistogramBucket {
    /// Inclusive seconds bound.
    pub le: BucketBound,
    /// Cumulative count.
    pub value: u64,
}
/// Queue-wait histogram in status JSON.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QueueHistogram {
    /// Host label.
    pub host: String,
    /// Class label.
    pub class: String,
    /// Cumulative buckets, including +Inf.
    pub buckets: Vec<HistogramBucket>,
    /// Sum in seconds.
    pub sum: f64,
    /// Observation count.
    pub count: u64,
}
/// Shared status representation of the existing metric families.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MetricsSnapshot {
    /// Completed/refused attempts by outcome.
    pub ix_build_requests_total: Vec<OutcomeCounter>,
    /// Abnormal attempts by stable code.
    pub ix_build_terminations_total: Vec<CodeCounter>,
    /// Per-request blocked durations by class/reason.
    pub ix_build_blocked_seconds_total: Vec<BlockedCounter>,
    /// Existing queue-wait histogram.
    pub ix_build_queue_wait_seconds: Vec<QueueHistogram>,
}

/// Additive, read-only request projection for monitoring consumers.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RequestProjection {
    /// Legacy queue entry timestamp.
    pub enqueued_at_ms: Option<u64>,
    /// Legacy admission timestamp.
    pub started_at_ms: Option<u64>,
    /// Legacy terminal timestamp.
    pub ended_at_ms: Option<u64>,
    /// Legacy queue duration.
    pub wait_ms: Option<u64>,
    /// Legacy reason classification.
    pub reason: Option<String>,
    /// Observed direct-child exit code.
    pub exit_code: Option<i32>,
    /// Observed direct-child signal.
    pub signal: Option<i32>,
    /// Attempt identity.
    pub request_id: String,
    /// Class label.
    pub class: String,
    /// Scheduling priority.
    pub priority: u32,
    /// Legacy phase.
    pub state: String,
    /// Canonical trace fields, flattened on the wire.
    #[serde(flatten)] pub trace: RequestTrace,
}
/// Additive, read-only event projection.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct EventProjection {
    /// Event sequence identity.
    pub event_id: String,
    /// Snapshot UTC timestamp.
    pub observed_at_ms: u64,
    /// transition, blocked, command_started, or refused.
    pub kind: String,
    /// Request identity/class/trace fields, flattened on the wire.
    #[serde(flatten)] pub request: RequestProjection,
}
/// Additive, read-only status envelope.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct StatusProjection {
    /// Existing status schema version.
    pub schema_version: Option<u32>,
    /// Snapshot UTC timestamp.
    pub observed_at_ms: Option<u64>,
    /// Number of events pruned from this source.
    pub pruned_events: Option<u64>,
    /// Store identity.
    pub store_id: String,
    /// Host identity.
    pub host: String,
    /// Request records.
    pub requests: Vec<RequestProjection>,
    /// Existing event stream.
    pub events: Vec<EventProjection>,
    /// Shared metrics snapshot.
    pub metrics: MetricsSnapshot,
}
