// SPDX-License-Identifier: MIT
//! Bounded build telemetry folded exactly once from durable transition evidence.

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::metrics::{BUCKETS_SECONDS, PriorityClass};
use crate::model::{Event, MemorySample, Phase, Request, State};
use locked_build::wire::{Code, MetricFamily, MetricKind, MetricSample, MetricValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const BYTES: &[f64] = &[
    1_000_000.,
    10_000_000.,
    100_000_000.,
    500_000_000.,
    1_000_000_000.,
    3_000_000_000.,
    8_000_000_000.,
    16_000_000_000.,
    32_000_000_000.,
    64_000_000_000.,
];
const RATIOS: &[f64] = &[0., 0.05, 0.1, 0.25, 0.5, 0.75, 0.9, 1., 2.];
const MAX_IDENTITIES: usize = 128;
const MAX_LANES: usize = 64;
const REASONS: &[&str] = &[
    "memory_budget",
    "fifo_or_priority_reservation",
    "units",
    "load",
    "disk",
    "memory_reserve",
    "telemetry_unavailable",
    "ready",
];
type Labels = BTreeMap<String, String>;

/// Measurements frozen in an event before its request can be pruned.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EventTelemetry {
    pub repo: String,
    pub subcommand: String,
    pub reservation_bytes: Option<u64>,
    pub limit_bytes: Option<u64>,
    pub hold_limit_ms: Option<u64>,
    pub memory_sample: Option<MemorySample>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}
impl EventTelemetry {
    pub(crate) fn identity(cwd: &Path, command: &[String]) -> Self {
        // Resolve worktree .git indirection to the common repository name.
        let repo = cwd
            .ancestors()
            .find_map(|dir| {
                let dotgit = dir.join(".git");
                if dotgit.is_dir() {
                    return dir.file_name().map(|n| n.to_string_lossy().into_owned());
                }
                let content = std::fs::read_to_string(&dotgit).ok()?;
                let gitdir = dir.join(content.trim().strip_prefix("gitdir: ")?);
                let common = gitdir.canonicalize().ok()?;
                common
                    .ancestors()
                    .find(|p| p.file_name().is_some_and(|n| n == ".git"))
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "other".into());
        let args: Vec<std::ffi::OsString> = command.iter().map(Into::into).collect();
        let subcommand = crate::sample::cargo_subcommand(&args, "cargo");
        Self {
            repo,
            subcommand: match subcommand {
                Some("build") => "build",
                Some("test") => "test",
                Some("check") => "check",
                Some("clippy") => "clippy",
                Some("xtask") => "xtask",
                _ => "other",
            }
            .into(),
            ..Self::default()
        }
    }
    pub(crate) fn for_request(request: &Request, config: &Config) -> Self {
        let mut data = request.telemetry.clone();
        data.reservation_bytes = request.started_at_ms.and(Some(request.reservation_bytes()));
        data.limit_bytes = request.memory_cap_bytes;
        data.hold_limit_ms = Some(config.hold_ms);
        data.memory_sample = request
            .lease_record
            .as_ref()
            .and_then(|lease| lease.memory_sample.clone());
        data.exit_code = request.exit_code;
        data.signal = request.signal;
        data
    }
}

/// Compact durable event ledger; label registries have lifetime caps.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Ledger {
    families: Vec<MetricFamily>,
    identities: BTreeSet<String>,
    lanes: BTreeSet<String>,
    /// Admission wait stays until terminal reconciliation; bounded by active requests.
    admitted_wait_ms: BTreeMap<String, u64>,
}
fn labels(values: &[(&str, &str)]) -> Labels {
    values
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect()
}
fn class(value: &str) -> &str {
    PriorityClass::from_class(value).label()
}
fn outcome(event: &Event) -> &'static str {
    if event.kind == "refused" {
        return "refused";
    }
    match event.state {
        Phase::Succeeded => "ok",
        Phase::Cancelled => "cancelled",
        Phase::TimedOut => "timed_out",
        _ => "failed",
    }
}
fn valid_lane(value: &str) -> bool {
    value.len() <= 120
        && value.split(':').count() == 3
        && value
            .split(':')
            .all(|s| !s.is_empty() && !s.chars().any(char::is_control))
}
fn bounded(value: &str) -> &str {
    if !value.is_empty() && value.len() <= 120 && !value.chars().any(char::is_control) {
        value
    } else {
        "other"
    }
}
impl Ledger {
    fn register_lane(&mut self, value: &str) -> String {
        if valid_lane(value) && (self.lanes.contains(value) || self.lanes.len() < MAX_LANES) {
            self.lanes.insert(value.into());
            value.into()
        } else {
            "other".into()
        }
    }
    fn lane(&self, value: &str, current: &BTreeSet<String>) -> String {
        if self.lanes.contains(value) || current.contains(value) {
            value.into()
        } else {
            "other".into()
        }
    }
    pub(crate) fn owner_counts(&self, state: &State, phase: Phase) -> BTreeMap<String, usize> {
        let current: BTreeSet<_> = state
            .requests
            .iter()
            .filter(|r| {
                !r.phase.terminal() && valid_lane(&r.owner) && !self.lanes.contains(&r.owner)
            })
            .map(|r| r.owner.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(MAX_LANES.saturating_sub(self.lanes.len()))
            .collect();
        let mut owners: BTreeMap<String, usize> = self
            .lanes
            .iter()
            .chain(&current)
            .map(|lane| (lane.clone(), 0))
            .collect();
        owners.insert("other".into(), 0);
        for request in state.requests.iter().filter(|r| r.phase == phase) {
            *owners
                .entry(self.lane(&request.owner, &current))
                .or_default() += 1;
        }
        owners
    }
    pub(crate) fn observe(&mut self, event: &Event, pre_reboot_wait_ms: u64) -> Result<()> {
        self.register_lane(&event.owner);
        let system_name = match event.kind.as_str() {
            "supervisor_restart" => Some("ix_build_supervisor_restarts_total"),
            "recovered" => Some("ix_build_recover_events_total"),
            "boot_changed" => Some("ix_build_boot_changes_total"),
            _ => None,
        };
        if let Some(name) = system_name {
            return self.counter(
                name,
                "Durable supervisor/recovery/boot events.",
                Labels::new(),
            );
        }
        if event.correction {
            return Ok(());
        }
        if event.state == Phase::Running {
            self.admitted_wait_ms
                .entry(event.request_id.0.clone())
                .or_insert(event.wait_ms.saturating_add(pre_reboot_wait_ms));
        }
        if !event.state.terminal() {
            return Ok(());
        }
        let seconds = BUCKETS_SECONDS.map(|bound| bound as f64);
        let wait_ms = self
            .admitted_wait_ms
            .remove(&event.request_id.0)
            .unwrap_or_else(|| event.wait_ms.saturating_add(pre_reboot_wait_ms));
        let base = labels(&[("class", class(&event.class)), ("outcome", outcome(event))]);
        self.histogram(
            "ix_build_wait_seconds",
            "Queue duration for every terminal request, including cancellations and refusals.",
            &base,
            wait_ms as f64 / 1000.,
            &seconds,
        )?;
        if let Some(hold) = event
            .hold_ms
            .filter(|_| event.reason.as_deref() != Some("host_rebooted"))
        {
            self.histogram(
                "ix_build_hold_seconds",
                "Admitted lease duration including command startup.",
                &base,
                hold as f64 / 1000.,
                &seconds,
            )?;
            if let Some(limit) = event.telemetry.hold_limit_ms.filter(|n| *n > 0) {
                self.histogram(
                    "ix_build_hold_limit_ratio",
                    "Lease hold duration divided by its policy hold_ms cap.",
                    &base,
                    hold as f64 / limit as f64,
                    RATIOS,
                )?;
            }
        }
        if event.trace.running_at_ms.is_some() && event.reason.as_deref() != Some("host_rebooted") {
            self.histogram(
                "ix_build_run_seconds",
                "Command execution duration, excluding queue and startup.",
                &base,
                event.trace.running_seconds,
                &seconds,
            )?;
        }
        if let Some(reserved) = event.telemetry.reservation_bytes {
            self.histogram(
                "ix_build_memory_reserved_bytes",
                "Admission reservation of completed leases.",
                &base,
                reserved as f64,
                BYTES,
            )?;
        }
        if let Some(sample) = &event.telemetry.memory_sample {
            let mut measured = base.clone();
            measured.insert("measurement".into(), sample.metric.as_str().into());
            self.histogram(
                "ix_build_memory_peak_bytes",
                "Peak observed command memory; watchdog peaks are sampled lower bounds.",
                &measured,
                sample.bytes as f64,
                BYTES,
            )?;
            if let Some(limit) = event.telemetry.limit_bytes.filter(|n| *n > 0) {
                self.histogram(
                    "ix_build_memory_headroom_ratio",
                    "One minus measured peak divided by hard cap; negative after an overshoot.",
                    &measured,
                    1. - sample.bytes as f64 / limit as f64,
                    RATIOS,
                )?;
            }
        }
        if let Some(code) = event.trace.diagnostic.code {
            let exit = match event.telemetry.exit_code {
                Some(101) => "101",
                Some(0) => "zero",
                Some(_) => "other",
                None => "none",
            };
            let signal = match event.telemetry.signal {
                Some(n @ 1..=64) => n.to_string(),
                Some(_) => "other".into(),
                None => "none".into(),
            };
            let cause = match code {
                Code::CommandExit | Code::ExternalSignal => "command",
                _ => "tool",
            };
            self.counter(
                "ix_build_failures_total",
                "Abnormal outcomes by code, exit class, signal and initiating cause.",
                labels(&[
                    ("class", class(&event.class)),
                    ("code", code.as_str()),
                    ("exit_class", exit),
                    ("signal", &signal),
                    ("cause", cause),
                ]),
            )?;
            if matches!(code, Code::RssCap | Code::OomKilled) {
                let mut kill = labels(&[("class", class(&event.class)), ("code", code.as_str())]);
                if let Some(limit) = event.telemetry.limit_bytes {
                    self.histogram(
                        "ix_build_memory_kill_limit_bytes",
                        "Hard memory limit on OOM/RSS kills.",
                        &kill,
                        limit as f64,
                        BYTES,
                    )?;
                }
                if let Some(sample) = &event.telemetry.memory_sample {
                    kill.insert("measurement".into(), sample.metric.as_str().into());
                    self.histogram(
                        "ix_build_memory_kill_measured_bytes",
                        "Measured peak at OOM/RSS kill, absent when unavailable.",
                        &kill,
                        sample.bytes as f64,
                        BYTES,
                    )?;
                }
            }
        }
        let lane = self.register_lane(&event.owner);
        let mut identity = labels(&[
            ("lane", &lane),
            ("ticket", bounded(&event.ticket)),
            ("repo", bounded(&event.telemetry.repo)),
            (
                "subcommand",
                match event.telemetry.subcommand.as_str() {
                    "build" => "build",
                    "test" => "test",
                    "check" => "check",
                    "clippy" => "clippy",
                    "xtask" => "xtask",
                    _ => "other",
                },
            ),
        ]);
        let key = serde_json::to_string(&identity)?;
        if self.identities.contains(&key) || self.identities.len() < MAX_IDENTITIES {
            self.identities.insert(key);
        } else {
            identity = labels(&[
                ("lane", "other"),
                ("ticket", "other"),
                ("repo", "other"),
                ("subcommand", "other"),
            ]);
        }
        identity.extend(base);
        self.counter(
            "ix_build_identity_total",
            "Terminal builds by bounded identity; at most 128 identities plus other per ledger.",
            identity,
        )
    }
    fn family(&mut self, name: &str, help: &str, kind: MetricKind) -> &mut MetricFamily {
        let index = self
            .families
            .iter()
            .position(|f| f.name == name)
            .unwrap_or_else(|| {
                self.families.push(MetricFamily {
                    name: name.into(),
                    help: help.into(),
                    kind,
                    samples: vec![],
                });
                self.families.len() - 1
            });
        &mut self.families[index]
    }
    fn counter(&mut self, name: &str, help: &str, labels: Labels) -> Result<()> {
        add(
            self.family(name, help, MetricKind::Counter),
            "",
            labels,
            MetricValue::Integer(1),
        )
    }
    fn histogram(
        &mut self,
        name: &str,
        help: &str,
        labels: &Labels,
        value: f64,
        bounds: &[f64],
    ) -> Result<()> {
        if !value.is_finite() {
            return Err(Error::new(
                ErrorCode::State,
                "non-finite telemetry measurement",
            ));
        }
        let family = self.family(name, help, MetricKind::Histogram);
        for bound in bounds.iter().copied().map(Some).chain([None]) {
            let mut bucket = labels.clone();
            bucket.insert(
                "le".into(),
                bound.map_or_else(|| "+Inf".into(), |n| n.to_string()),
            );
            add(
                family,
                "_bucket",
                bucket,
                MetricValue::Integer(u64::from(bound.is_none_or(|n| value <= n))),
            )?;
        }
        add(family, "_count", labels.clone(), MetricValue::Integer(1))?;
        add(family, "_sum", labels.clone(), MetricValue::Number(value))
    }
    pub(crate) fn snapshot(
        &self,
        state: &State,
        config: &Config,
        now: u64,
        clock: u64,
        pre_reboot_wait_ms: &BTreeMap<String, u64>,
    ) -> Vec<MetricFamily> {
        let mut families = self.families.clone();
        for name in [
            "ix_build_supervisor_restarts_total",
            "ix_build_recover_events_total",
            "ix_build_boot_changes_total",
        ] {
            if !families.iter().any(|f| f.name == name) {
                families.push(MetricFamily {
                    name: name.into(),
                    help: "Durable supervisor/recovery/boot events.".into(),
                    kind: MetricKind::Counter,
                    samples: vec![MetricSample {
                        suffix: String::new(),
                        labels: Labels::new(),
                        value: MetricValue::Integer(0),
                    }],
                });
            }
        }
        let running: Vec<_> = state
            .requests
            .iter()
            .filter(|r| r.phase == Phase::Running)
            .collect();
        let queued: Vec<_> = state
            .requests
            .iter()
            .filter(|r| r.phase == Phase::Queued)
            .collect();
        let mut gauge = |name: &str, help: &str, labels: Labels, value: f64| {
            if let Some(f) = families.iter_mut().find(|f| f.name == name) {
                f.samples.push(MetricSample {
                    suffix: String::new(),
                    labels,
                    value: MetricValue::Number(value),
                });
            } else {
                families.push(MetricFamily {
                    name: name.into(),
                    help: help.into(),
                    kind: MetricKind::Gauge,
                    samples: vec![MetricSample {
                        suffix: String::new(),
                        labels,
                        value: MetricValue::Number(value),
                    }],
                });
            }
        };
        for (name, value) in [
            ("ix_build_running", running.len() as f64),
            ("ix_build_units_used", f64::from(state.used())),
            ("ix_build_units_capacity", f64::from(config.capacity_units)),
            (
                "ix_build_memory_budget_used_bytes",
                state.memory_used(config) as f64,
            ),
            (
                "ix_build_memory_budget_total_bytes",
                config.memory_budget_bytes.unwrap_or(0) as f64,
            ),
            (
                "ix_build_oldest_queued_seconds",
                queued
                    .iter()
                    .map(|r| {
                        clock
                            .saturating_sub(r.enqueued_clock_ms)
                            .saturating_add(pre_reboot_wait_ms.get(&r.id.0).copied().unwrap_or(0))
                    })
                    .max()
                    .unwrap_or(0) as f64
                    / 1000.,
            ),
            (
                "ix_build_policy_hold_seconds",
                config.hold_ms as f64 / 1000.,
            ),
            (
                "ix_build_policy_jobs",
                f64::from(config.cargo_build_jobs.unwrap_or(0)),
            ),
            (
                "ix_build_policy_test_threads",
                f64::from(config.rust_test_threads.unwrap_or(0)),
            ),
            (
                "ix_build_policy_memory_reserve_bytes",
                config.memory_reserve_bytes as f64,
            ),
            (
                "ix_build_policy_memory_reserve_per_job_bytes",
                config.memory_reserve_per_job_bytes as f64,
            ),
            (
                "ix_build_policy_memory_limit_per_job_bytes",
                config.memory_limit_per_job_bytes as f64,
            ),
            ("ix_build_policy_maximum_load", config.maximum_load),
            (
                "ix_build_policy_disk_reserve_bytes",
                config.disk_reserve_bytes as f64,
            ),
            (
                "ix_build_policy_maximum_units",
                f64::from(config.maximum_units),
            ),
            ("ix_build_export_timestamp_seconds", now as f64 / 1000.),
        ] {
            gauge(
                name,
                "Current host capacity or effective policy (zero means unset for optional values).",
                Labels::new(),
                value,
            );
        }
        let headrooms: Vec<_> = running
            .iter()
            .filter_map(|r| {
                let cap = r.memory_cap_bytes.filter(|n| *n > 0)?;
                let peak = r.lease_record.as_ref()?.memory_sample.as_ref()?.bytes;
                Some(1. - peak as f64 / cap as f64)
            })
            .collect();
        gauge(
            "ix_build_memory_measurements_available",
            "Running leases with an observed memory peak and hard cap.",
            Labels::new(),
            headrooms.len() as f64,
        );
        if let Some(minimum) = headrooms.into_iter().reduce(f64::min) {
            gauge(
                "ix_build_running_memory_headroom_ratio",
                "Minimum observed running lease headroom; absent when no measurement exists.",
                Labels::new(),
                minimum,
            );
        }
        for priority in PriorityClass::ALL {
            let priority = priority.label();
            for (name, value) in [
                (
                    "ix_build_policy_class_memory_reserve_per_job_bytes",
                    config
                        .memory_reserve_per_job_by_class
                        .get(priority)
                        .copied()
                        .unwrap_or(config.memory_reserve_per_job_bytes),
                ),
                (
                    "ix_build_policy_class_memory_limit_per_job_bytes",
                    config
                        .memory_per_job_by_class
                        .get(priority)
                        .copied()
                        .unwrap_or(config.memory_limit_per_job_bytes),
                ),
                (
                    "ix_build_policy_class_memory_cap_bytes",
                    config
                        .memory_cap_by_class
                        .get(priority)
                        .copied()
                        .unwrap_or(0),
                ),
            ] {
                gauge(
                    name,
                    "Effective class memory policy; zero cap means unset.",
                    labels(&[("class", priority)]),
                    value as f64,
                );
            }
            for reason in REASONS {
                let count = queued
                    .iter()
                    .filter(|r| {
                        class(&r.class) == priority
                            && if *reason == "ready" {
                                r.active_blocked_reasons.is_empty()
                            } else {
                                r.active_blocked_reasons
                                    .iter()
                                    .any(|v| queue_reason(v) == *reason)
                            }
                    })
                    .count();
                gauge(
                    "ix_build_queue_depth_by_reason",
                    "Queued requests by class and last observed blocking reason; simultaneous reasons overlap.",
                    labels(&[("class", priority), ("reason", reason)]),
                    count as f64,
                );
            }
        }
        for (owner, count) in self.owner_counts(state, Phase::Queued) {
            gauge(
                "ix_build_owner_queued",
                "Queued depth by lifetime-bounded owner lane.",
                labels(&[("lane", &owner)]),
                count as f64,
            );
        }
        let policy_hash = policy_hash(config);
        gauge(
            "ix_build_info",
            "Exporter version and effective policy fingerprint (FNV-1a, not a security digest).",
            labels(&[
                ("version", env!("CARGO_PKG_VERSION")),
                ("policy_hash", &policy_hash),
            ]),
            1.,
        );
        for family in &mut families {
            for sample in &mut family.samples {
                sample.labels.insert("host".into(), state.host.clone());
            }
        }
        families
    }
}
fn queue_reason(value: &str) -> &str {
    match value {
        "priority_reservation" => "fifo_or_priority_reservation",
        "capacity_occupied" | "capacity_shrunk" => "units",
        v if REASONS.contains(&v) => v,
        _ => "telemetry_unavailable",
    }
}
fn policy_hash(config: &Config) -> String {
    let bytes = serde_json::to_vec(config).unwrap_or_default();
    let hash = bytes.iter().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}
fn add(family: &mut MetricFamily, suffix: &str, labels: Labels, delta: MetricValue) -> Result<()> {
    if let Some(sample) = family
        .samples
        .iter_mut()
        .find(|s| s.suffix == suffix && s.labels == labels)
    {
        match (&mut sample.value, delta) {
            (MetricValue::Integer(n), MetricValue::Integer(d)) => {
                *n = n
                    .checked_add(d)
                    .ok_or_else(|| Error::new(ErrorCode::State, "telemetry counter overflow"))?
            }
            (MetricValue::Number(n), MetricValue::Number(d)) => {
                *n += d;
                if !n.is_finite() {
                    return Err(Error::new(ErrorCode::State, "telemetry sum overflow"));
                }
            }
            _ => {
                return Err(Error::new(
                    ErrorCode::State,
                    "telemetry sample type changed",
                ));
            }
        }
    } else {
        family.samples.push(MetricSample {
            suffix: suffix.into(),
            labels,
            value: delta,
        });
    }
    Ok(())
}

pub(crate) fn render(families: &[MetricFamily], output: &mut String) {
    for family in families {
        output.push_str(&format!(
            "# HELP {} {}\n# TYPE {} {}\n",
            family.name,
            family.help,
            family.name,
            family.kind.as_str()
        ));
        for sample in &family.samples {
            let labels = sample
                .labels
                .iter()
                .map(|(k, v)| format!("{k}=\"{}\"", crate::metrics::escape_label(v)))
                .collect::<Vec<_>>()
                .join(",");
            output.push_str(&format!(
                "{}{}{{{labels}}} {}\n",
                family.name, sample.suffix, sample.value
            ));
        }
    }
}
