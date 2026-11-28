// SPDX-License-Identifier: MIT
//! Shared error, queue and metrics records for producers and monitoring readers.

use serde::{Deserialize, Serialize};

/// Encoding support for a store shared with pre-rename readers.
pub mod persistence;

/// Stable abnormal-outcome codes. Spellings are part of the monitoring contract.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize)]
pub enum Code {
    /// Illegal control-plane state transition.
    #[serde(rename = "IB_INVALID_TRANSITION", alias = "LB_INVALID_TRANSITION")]
    InvalidTransition,
    /// Store requires a newer reader.
    #[serde(rename = "IB_STORE_VERSION", alias = "LB_STORE_VERSION")]
    StoreVersion,
    /// Policy was edited outside the audited command.
    #[serde(rename = "IB_POLICY_MODIFIED", alias = "LB_POLICY_MODIFIED")]
    PolicyModified,
    /// Watchdog memory limit.
    #[serde(rename = "IB_RSS_CAP", alias = "LB_RSS_CAP")]
    RssCap,
    /// Kernel cgroup OOM kill.
    #[serde(rename = "IB_OOM_KILLED", alias = "LB_OOM_KILLED")]
    OomKilled,
    /// Maximum lease duration.
    #[serde(rename = "IB_HOLD_TIMEOUT", alias = "LB_HOLD_TIMEOUT")]
    HoldTimeout,
    /// Supervision was lost or degraded.
    #[serde(rename = "IB_SUPERVISION_LOST", alias = "LB_SUPERVISION_LOST")]
    SupervisionLost,
    /// Command target preparation failed before the command started.
    #[serde(rename = "IB_TARGET_UNAVAILABLE", alias = "LB_TARGET_UNAVAILABLE")]
    TargetUnavailable,
    /// Requested memory exceeds the host budget.
    #[serde(
        rename = "IB_REFUSED_MEMORY_BUDGET",
        alias = "LB_REFUSED_MEMORY_BUDGET"
    )]
    RefusedMemoryBudget,
    /// Requested memory exceeds the class cap.
    #[serde(rename = "IB_REFUSED_CLASS_CAP", alias = "LB_REFUSED_CLASS_CAP")]
    RefusedClassCap,
    /// Queue or unit limit.
    #[serde(rename = "IB_REFUSED_QUEUE", alias = "LB_REFUSED_QUEUE")]
    RefusedQueue,
    /// Required memory enforcement is unavailable.
    #[serde(
        rename = "IB_REFUSED_CAP_UNAVAILABLE",
        alias = "LB_REFUSED_CAP_UNAVAILABLE"
    )]
    RefusedCapUnavailable,
    /// Caller cancellation.
    #[serde(rename = "IB_CANCELLED", alias = "LB_CANCELLED")]
    Cancelled,
    /// A signal was not initiated by ix-build.
    #[serde(rename = "IB_EXTERNAL_SIGNAL", alias = "LB_EXTERNAL_SIGNAL")]
    ExternalSignal,
    /// Nonzero command exit.
    #[serde(rename = "IB_COMMAND_EXIT", alias = "LB_COMMAND_EXIT")]
    CommandExit,
    /// Invalid request.
    #[serde(rename = "IB_INVALID_REQUEST", alias = "LB_INVALID_REQUEST")]
    InvalidRequest,
    /// State or configuration is unavailable.
    #[serde(rename = "IB_STATE_UNAVAILABLE", alias = "LB_STATE_UNAVAILABLE")]
    StateUnavailable,
}

impl Code {
    /// Every supported code, for bounded metrics and exhaustive validation.
    pub const ALL: [Self; 17] = [
        Self::InvalidTransition,
        Self::StoreVersion,
        Self::PolicyModified,
        Self::RssCap,
        Self::OomKilled,
        Self::HoldTimeout,
        Self::SupervisionLost,
        Self::TargetUnavailable,
        Self::RefusedMemoryBudget,
        Self::RefusedClassCap,
        Self::RefusedQueue,
        Self::RefusedCapUnavailable,
        Self::Cancelled,
        Self::ExternalSignal,
        Self::CommandExit,
        Self::InvalidRequest,
        Self::StateUnavailable,
    ];
    /// Stable wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidTransition => "IB_INVALID_TRANSITION",
            Self::StoreVersion => "IB_STORE_VERSION",
            Self::PolicyModified => "IB_POLICY_MODIFIED",
            Self::RssCap => "IB_RSS_CAP",
            Self::OomKilled => "IB_OOM_KILLED",
            Self::HoldTimeout => "IB_HOLD_TIMEOUT",
            Self::SupervisionLost => "IB_SUPERVISION_LOST",
            Self::TargetUnavailable => "IB_TARGET_UNAVAILABLE",
            Self::RefusedMemoryBudget => "IB_REFUSED_MEMORY_BUDGET",
            Self::RefusedClassCap => "IB_REFUSED_CLASS_CAP",
            Self::RefusedQueue => "IB_REFUSED_QUEUE",
            Self::RefusedCapUnavailable => "IB_REFUSED_CAP_UNAVAILABLE",
            Self::Cancelled => "IB_CANCELLED",
            Self::ExternalSignal => "IB_EXTERNAL_SIGNAL",
            Self::CommandExit => "IB_COMMAND_EXIT",
            Self::InvalidRequest => "IB_INVALID_REQUEST",
            Self::StateUnavailable => "IB_STATE_UNAVAILABLE",
        }
    }
}

impl Serialize for Code {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let spelling = if persistence::active() {
            self.legacy_spelling()
        } else {
            self.as_str()
        };
        serializer.serialize_str(spelling)
    }
}
impl Code {
    /// Oldest deployed reader's equivalent; exact new diagnostics live in the extension ledger.
    pub fn legacy_equivalent(self) -> Self {
        match self {
            Self::InvalidTransition
            | Self::StoreVersion
            | Self::PolicyModified
            | Self::TargetUnavailable => Self::StateUnavailable,
            Self::RssCap
            | Self::OomKilled
            | Self::HoldTimeout
            | Self::SupervisionLost
            | Self::RefusedMemoryBudget
            | Self::RefusedClassCap
            | Self::RefusedQueue
            | Self::RefusedCapUnavailable
            | Self::Cancelled
            | Self::ExternalSignal
            | Self::CommandExit
            | Self::InvalidRequest
            | Self::StateUnavailable => self,
        }
    }
    fn legacy_spelling(self) -> &'static str {
        match self {
            Self::InvalidTransition
            | Self::StoreVersion
            | Self::PolicyModified
            | Self::TargetUnavailable
            | Self::StateUnavailable => "LB_STATE_UNAVAILABLE",
            Self::RssCap => "LB_RSS_CAP",
            Self::OomKilled => "LB_OOM_KILLED",
            Self::HoldTimeout => "LB_HOLD_TIMEOUT",
            Self::SupervisionLost => "LB_SUPERVISION_LOST",
            Self::RefusedMemoryBudget => "LB_REFUSED_MEMORY_BUDGET",
            Self::RefusedClassCap => "LB_REFUSED_CLASS_CAP",
            Self::RefusedQueue => "LB_REFUSED_QUEUE",
            Self::RefusedCapUnavailable => "LB_REFUSED_CAP_UNAVAILABLE",
            Self::Cancelled => "LB_CANCELLED",
            Self::ExternalSignal => "LB_EXTERNAL_SIGNAL",
            Self::CommandExit => "LB_COMMAND_EXIT",
            Self::InvalidRequest => "LB_INVALID_REQUEST",
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
        match self {
            Self::Integer(value) => write!(out, "{value}"),
            Self::Number(value) => write!(out, "{value}"),
            Self::Boolean(value) => write!(out, "{value}"),
            Self::Text(value) => out.write_str(value),
        }
    }
}
impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Self::Integer(value)
    }
}
impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Self::Integer(u64::from(value))
    }
}
impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}
impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}
impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

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
    pub fn failure(
        code: Code,
        request: &str,
        key: &str,
        limit: Value,
        measured: Value,
        what: &str,
        killed: &str,
    ) -> Self {
        let what = what.split_whitespace().collect::<Vec<_>>().join(" ");
        let killed = killed.split_whitespace().collect::<Vec<_>>().join("_");
        let message = format!(
            "ix-build: {} request={request} limit={key}={limit} measured={measured} ({what}) killed={killed}",
            code.as_str()
        );
        Self {
            code: Some(code),
            message: Some(message),
            limit_key: Some(key.to_owned()),
            limit_value: Some(limit),
            measured_value: Some(measured),
            measured_what: Some(what),
            killed: Some(killed),
        }
    }
}

/// One cumulative queued blocking reason; simultaneous reasons may overlap.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BlockObservation {
    /// Closed reason vocabulary from ix-build-wire.md.
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
    #[serde(flatten)]
    pub diagnostic: Diagnostic,
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
    /// Requested hard memory limit; this legacy name is not a reservation.
    pub requested_memory_bytes: Option<u64>,
    /// Granted hard memory limit; this legacy name is not a reservation.
    pub granted_memory_bytes: Option<u64>,
    /// Requested admission reservation, separate from the hard-cap aliases.
    pub requested_memory_reservation_bytes: Option<u64>,
    /// Granted admission reservation, separate from the hard-cap aliases.
    pub granted_memory_reservation_bytes: Option<u64>,
}

/// Shared status representation of the existing metric families.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MetricsSnapshot {
    /// Durable build telemetry and current capacity/policy gauges.
    pub telemetry: Vec<MetricFamily>,
}

/// Additive, read-only request projection for monitoring consumers.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RequestProjection {
    /// Memory required for admission, exposed by status.
    pub memory_needed_bytes: Option<u64>,
    /// Currently unreserved host memory budget, exposed by status.
    pub memory_free_budget_bytes: Option<u64>,
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
    #[serde(flatten)]
    pub trace: RequestTrace,
}
/// Additive, read-only event projection.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct EventProjection {
    /// This event corrects an earlier outcome of the same attempt.
    pub correction: bool,
    /// Event sequence identity.
    pub event_id: String,
    /// Snapshot UTC timestamp.
    pub observed_at_ms: u64,
    /// transition, blocked, command_started, or refused.
    pub kind: String,
    /// Request identity/class/trace fields, flattened on the wire.
    #[serde(flatten)]
    pub request: RequestProjection,
}
/// Additive, read-only status envelope.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct StatusProjection {
    /// Existing status schema version.
    pub schema_version: Option<u32>,
    /// Snapshot UTC timestamp.
    pub observed_at_ms: Option<u64>,
    /// Number of events pruned from this source.
    pub pruned_events: Option<u64>,
    /// Store identity.
    #[serde(default)]
    pub store_id: String,
    /// Host identity.
    #[serde(default)]
    pub host: String,
    /// Request records.
    pub requests: Vec<RequestProjection>,
    /// Existing event stream.
    pub events: Vec<EventProjection>,
    /// Shared metrics snapshot.
    #[serde(default)]
    pub metrics: MetricsSnapshot,
}

#[cfg(test)]
mod tests {
    /// Trace: FR-005-AC-8, FR-015-AC-3. Provenance: SWM-431.
    #[test]
    fn renamed_codes_emit_ib_and_read_existing_ledger_codes() {
        for code in super::Code::ALL {
            let name = code.as_str();
            assert!(name.starts_with("IB_"));
            let legacy = name.replacen("IB_", "LB_", 1);
            let decoded = serde_json::from_value::<super::Code>(serde_json::Value::String(legacy));
            assert_eq!(decoded.ok(), Some(code));
            assert_eq!(
                serde_json::to_value(code).ok(),
                Some(serde_json::Value::String(name.into()))
            );
        }
    }
    use super::*;

    /// Trace: FR-005-AC-8, FR-015-AC-3
    #[test]
    fn every_code_has_a_stable_complete_hand_written_diagnostic() {
        let cases = [
            (
                Code::InvalidTransition,
                "control.transition_valid",
                "true",
                "false",
                "illegal transition",
                "ix-build: IB_INVALID_TRANSITION request=host:r7 limit=control.transition_valid=true measured=false (illegal transition) killed=pgid:42/cargo",
            ),
            (
                Code::StoreVersion,
                "store.min_reader_version",
                "2",
                "1",
                "reader too old",
                "ix-build: IB_STORE_VERSION request=host:r7 limit=store.min_reader_version=2 measured=1 (reader too old) killed=pgid:42/cargo",
            ),
            (
                Code::PolicyModified,
                "policy.sha256",
                "audited",
                "modified",
                "hand edit",
                "ix-build: IB_POLICY_MODIFIED request=host:r7 limit=policy.sha256=audited measured=modified (hand edit) killed=pgid:42/cargo",
            ),
            (
                Code::RssCap,
                "memory_per_job_bytes*parallelism",
                "24000000000",
                "25000000000",
                "rss_bytes",
                "ix-build: IB_RSS_CAP request=host:r7 limit=memory_per_job_bytes*parallelism=24000000000 measured=25000000000 (rss_bytes) killed=pgid:42/cargo",
            ),
            (
                Code::OomKilled,
                "memory_per_job_bytes*parallelism",
                "24000000000",
                "1",
                "cgroup oom_kill",
                "ix-build: IB_OOM_KILLED request=host:r7 limit=memory_per_job_bytes*parallelism=24000000000 measured=1 (cgroup oom_kill) killed=pgid:42/cargo",
            ),
            (
                Code::HoldTimeout,
                "hold_ms",
                "5400000",
                "5400100",
                "elapsed_ms",
                "ix-build: IB_HOLD_TIMEOUT request=host:r7 limit=hold_ms=5400000 measured=5400100 (elapsed_ms) killed=pgid:42/cargo",
            ),
            (
                Code::SupervisionLost,
                "supervision_required",
                "true",
                "false",
                "watchdog absent",
                "ix-build: IB_SUPERVISION_LOST request=host:r7 limit=supervision_required=true measured=false (watchdog absent) killed=pgid:42/cargo",
            ),
            (
                Code::TargetUnavailable,
                "command.target_writable",
                "true",
                "false",
                "ReadOnlyFilesystem",
                "ix-build: IB_TARGET_UNAVAILABLE request=host:r7 limit=command.target_writable=true measured=false (ReadOnlyFilesystem) killed=pgid:42/cargo",
            ),
            (
                Code::RefusedMemoryBudget,
                "memory_budget_bytes",
                "49400000000",
                "54000000000",
                "requested memory bytes",
                "ix-build: IB_REFUSED_MEMORY_BUDGET request=host:r7 limit=memory_budget_bytes=49400000000 measured=54000000000 (requested memory bytes) killed=pgid:42/cargo",
            ),
            (
                Code::RefusedClassCap,
                "memory_cap_by_class.point",
                "24000000000",
                "30000000000",
                "requested memory bytes",
                "ix-build: IB_REFUSED_CLASS_CAP request=host:r7 limit=memory_cap_by_class.point=24000000000 measured=30000000000 (requested memory bytes) killed=pgid:42/cargo",
            ),
            (
                Code::RefusedQueue,
                "MAX_REQUESTS",
                "100000",
                "100000",
                "retained requests",
                "ix-build: IB_REFUSED_QUEUE request=host:r7 limit=MAX_REQUESTS=100000 measured=100000 (retained requests) killed=pgid:42/cargo",
            ),
            (
                Code::RefusedCapUnavailable,
                "on_missing_cgroup",
                "refuse",
                "unavailable",
                "memory cap backend",
                "ix-build: IB_REFUSED_CAP_UNAVAILABLE request=host:r7 limit=on_missing_cgroup=refuse measured=unavailable (memory cap backend) killed=pgid:42/cargo",
            ),
            (
                Code::Cancelled,
                "run_permitted",
                "true",
                "false",
                "cancelled by caller",
                "ix-build: IB_CANCELLED request=host:r7 limit=run_permitted=true measured=false (cancelled by caller) killed=pgid:42/cargo",
            ),
            (
                Code::ExternalSignal,
                "external_signals",
                "none",
                "15",
                "terminated by signal 15 from outside ix-build",
                "ix-build: IB_EXTERNAL_SIGNAL request=host:r7 limit=external_signals=none measured=15 (terminated by signal 15 from outside ix-build) killed=pgid:42/cargo",
            ),
            (
                Code::CommandExit,
                "success_exit_code",
                "0",
                "101",
                "command exit status",
                "ix-build: IB_COMMAND_EXIT request=host:r7 limit=success_exit_code=0 measured=101 (command exit status) killed=pgid:42/cargo",
            ),
            (
                Code::InvalidRequest,
                "valid_request",
                "true",
                "false",
                "invalid request",
                "ix-build: IB_INVALID_REQUEST request=host:r7 limit=valid_request=true measured=false (invalid request) killed=pgid:42/cargo",
            ),
            (
                Code::StateUnavailable,
                "state_access",
                "read_write",
                "unavailable",
                "state inaccessible",
                "ix-build: IB_STATE_UNAVAILABLE request=host:r7 limit=state_access=read_write measured=unavailable (state inaccessible) killed=pgid:42/cargo",
            ),
        ];
        assert_eq!(cases.map(|case| case.0), Code::ALL);
        for (code, key, limit, measured, what, expected) in cases {
            let diagnostic = Diagnostic::failure(
                code,
                "host:r7",
                key,
                limit.into(),
                measured.into(),
                what,
                "pgid:42/cargo",
            );
            assert_eq!(diagnostic.message.as_deref(), Some(expected));
            assert!(
                diagnostic.limit_key.is_some()
                    && diagnostic.limit_value.is_some()
                    && diagnostic.measured_value.is_some()
            );
            let json = serde_json::to_value(&diagnostic).unwrap_or_default();
            assert_eq!(json["code"], code.as_str());
        }
    }

    /// Trace: FR-015-AC-3
    #[test]
    fn monitoring_envelope_rejects_missing_records_but_accepts_additive_fields() {
        assert!(serde_json::from_str::<StatusProjection>(r#"{"events":[]}"#).is_err());
        assert!(serde_json::from_str::<StatusProjection>(r#"{"requests":[]}"#).is_err());
        let status =
            serde_json::from_str::<StatusProjection>(r#"{"requests":[],"events":[],"future":42}"#);
        assert!(status.is_ok());
    }
}

/// Numeric Prometheus sample, preserving integer counters in the durable ledger.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MetricValue {
    /// Exact durable count.
    Integer(u64),
    /// Finite measured value.
    Number(f64),
}
impl std::fmt::Display for MetricValue {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Integer(n) => n.fmt(out),
            Self::Number(n) => n.fmt(out),
        }
    }
}
/// Prometheus metric family type.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    /// Monotonic cumulative count.
    Counter,
    /// Snapshot measurement.
    Gauge,
    /// Cumulative buckets, sum and count.
    Histogram,
}
impl MetricKind {
    /// Prometheus text spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
        }
    }
}
/// One sample of a family, shared by status, persistence and the exporter.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricSample {
    /// Empty for scalar families, or _bucket, _sum, _count for histograms.
    pub suffix: String,
    /// Bounded label values. Request IDs and commands are excluded.
    pub labels: std::collections::BTreeMap<String, String>,
    /// Sample value.
    pub value: MetricValue,
}
/// A complete additional telemetry family in status JSON.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricFamily {
    /// Stable ix_build_* name.
    pub name: String,
    /// Prometheus HELP text.
    pub help: String,
    /// Sample semantics.
    pub kind: MetricKind,
    /// Scalar samples or histogram bucket/sum/count samples.
    pub samples: Vec<MetricSample>,
}
