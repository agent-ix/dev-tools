// SPDX-License-Identifier: MIT
//! Bounded build telemetry folded exactly once from durable transition evidence.

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::metrics::{BUCKETS_SECONDS, PriorityClass};
use crate::model::{Event, MemorySample, Phase, Request, State};
use ix_build::wire::{Code, MetricFamily, MetricKind, MetricSample, MetricValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const BYTES: &[f64] = &[
    100_000_000.,
    1_000_000_000.,
    8_000_000_000.,
    32_000_000_000.,
];
const MAX_REPOS: usize = 30;
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

/// Compact durable event ledger; repositories and lanes have independent lifetime bounds.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Ledger {
    families: Vec<MetricFamily>,
    #[serde(alias = "identities")]
    repos: BTreeSet<String>,
    #[serde(default)]
    format_version: Option<u32>,
    lanes: BTreeSet<String>,
    /// Admission wait stays until terminal reconciliation; bounded by active requests.
    admitted_wait_ms: BTreeMap<String, u64>,
}
impl Default for Ledger {
    fn default() -> Self {
        Self {
            families: vec![],
            repos: BTreeSet::new(),
            lanes: BTreeSet::new(),
            admitted_wait_ms: BTreeMap::new(),
            format_version: Some(2),
        }
    }
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
    // Upgrade stored counters once. Changed histogram boundaries start a new epoch.
    // This preserves pruned lifetime totals without exporting retired families.
    fn upgrade(&mut self) -> Result<()> {
        if self.format_version == Some(2) {
            return Ok(());
        }
        if self.format_version.is_some() {
            return Err(Error::new(
                ErrorCode::State,
                "unsupported telemetry ledger format",
            ));
        }
        let previous = std::mem::take(&mut self.families);
        self.repos.clear();
        for family in previous {
            if matches!(family.kind, MetricKind::Histogram) {
                continue;
            }
            for sample in family.samples {
                let mut labels = sample.labels;
                labels.remove("host");
                if family.name == "ix_build_identity_total" {
                    if let (Some(class), Some(outcome)) =
                        (labels.get("class"), labels.get("outcome"))
                    {
                        let build_labels =
                            crate::telemetry::labels(&[("class", class), ("outcome", outcome)]);
                        add(
                            self.family(
                                "ix_build_builds_total",
                                "Terminal attempts by class and outcome.",
                                MetricKind::Counter,
                            ),
                            "",
                            build_labels,
                            sample.value.clone(),
                        )?;
                    }
                    labels.remove("class");
                    labels.remove("lane");
                    labels.remove("ticket");
                    if let Some(outcome) = labels.get_mut("outcome")
                        && outcome != "ok"
                        && outcome != "failed"
                    {
                        *outcome = "other".into();
                    }
                    if let Some(repo) = labels.get_mut("repo") {
                        if self.repos.contains(repo) || self.repos.len() < MAX_REPOS {
                            self.repos.insert(repo.clone());
                        } else {
                            *repo = "other".into();
                        }
                    }
                } else if family.name == "ix_build_failures_total" {
                    labels.retain(|key, _| key == "class" || key == "code");
                }
                let help = match family.name.as_str() {
                    "ix_build_identity_total" => {
                        "Terminal attempts by repository, subcommand and ok/failed/other outcome."
                    }
                    "ix_build_failures_total" => "Abnormal outcomes by stable diagnostic code.",
                    _ => &family.help,
                };
                add(
                    self.family(&family.name, help, family.kind),
                    &sample.suffix,
                    labels,
                    sample.value,
                )?;
            }
        }
        self.format_version = Some(2);
        Ok(())
    }
    fn register_lane(&mut self, value: &str) -> String {
        if valid_lane(value) && (self.lanes.contains(value) || self.lanes.len() < MAX_LANES) {
            self.lanes.insert(value.into());
            value.into()
        } else {
            "other".into()
        }
    }
    pub(crate) fn observe(&mut self, event: &Event, pre_reboot_wait_ms: u64) -> Result<()> {
        self.upgrade()?;
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
        let base = labels(&[("class", class(&event.class))]);
        self.counter(
            "ix_build_builds_total",
            "Terminal attempts by class and outcome.",
            labels(&[("class", class(&event.class)), ("outcome", outcome(event))]),
        )?;
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
            let lane = self.register_lane(&event.owner);
            add(
                self.family(
                    "ix_build_lane_unit_seconds_total",
                    "Completed admitted unit-seconds by bounded lane.",
                    MetricKind::Counter,
                ),
                "",
                labels(&[("lane", &lane)]),
                MetricValue::Number(hold as f64 / 1000. * f64::from(event.units)),
            )?;
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
        let wasted_cause = if event.state == Phase::Cancelled {
            Some("cancelled")
        } else if matches!(
            event.trace.diagnostic.code,
            Some(Code::RssCap | Code::OomKilled | Code::HoldTimeout)
        ) {
            Some("late_kill")
        } else {
            None
        };
        if let Some(cause) = wasted_cause
            && event.trace.running_at_ms.is_some()
            && event.reason.as_deref() != Some("host_rebooted")
        {
            add(
                self.family(
                    "ix_build_wasted_run_seconds_total",
                    "Measured execution seconds lost to cancellation or limit kills.",
                    MetricKind::Counter,
                ),
                "",
                labels(&[("class", class(&event.class)), ("cause", cause)]),
                MetricValue::Number(event.trace.running_seconds),
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
            let measured = base.clone();

            self.histogram(
                "ix_build_memory_peak_bytes",
                "Peak observed command memory; watchdog peaks are sampled lower bounds.",
                &measured,
                sample.bytes as f64,
                BYTES,
            )?;
        }
        if let Some(code) = event.trace.diagnostic.code {
            self.counter(
                "ix_build_failures_total",
                "Abnormal outcomes by stable diagnostic code.",
                labels(&[("class", class(&event.class)), ("code", code.as_str())]),
            )?;
        }

        let repo = bounded(&event.telemetry.repo);
        let repo = if self.repos.contains(repo) || self.repos.len() < MAX_REPOS {
            self.repos.insert(repo.into());
            repo
        } else {
            "other"
        };
        let subcommand = match event.telemetry.subcommand.as_str() {
            "build" => "build",
            "test" => "test",
            "check" => "check",
            "clippy" => "clippy",
            "xtask" => "xtask",
            _ => "other",
        };
        let identity = labels(&[("repo", repo), ("subcommand", subcommand)]);
        let mut completed = identity.clone();
        completed.insert(
            "outcome".into(),
            match outcome(event) {
                "ok" => "ok",
                "failed" => "failed",
                _ => "other",
            }
            .into(),
        );
        self.counter(
            "ix_build_identity_total",
            "Terminal attempts by repository, subcommand and ok/failed/other outcome.",
            completed,
        )?;
        if event.trace.running_at_ms.is_some() && event.reason.as_deref() != Some("host_rebooted") {
            add(
                self.family(
                    "ix_build_repo_run_seconds_total",
                    "Command seconds by repository and subcommand; no buckets.",
                    MetricKind::Counter,
                ),
                "",
                identity.clone(),
                MetricValue::Number(event.trace.running_seconds),
            )?;
            self.counter(
                "ix_build_repo_run_count_total",
                "Measured command runs by repository and subcommand; no buckets.",
                identity,
            )?;
        }
        Ok(())
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
    ) -> Result<Vec<MetricFamily>> {
        let mut normalized = self.clone();
        normalized.upgrade()?;
        let mut families = normalized.families;
        for name in [
            "ix_build_supervisor_restarts_total",
            "ix_build_recover_events_total",
            "ix_build_boot_changes_total",
            "ix_build_telemetry_dropped_total",
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
        let mut active_memory = BTreeMap::<(&str, &str), (u64, u64)>::new();
        if let Some(head) = state.next(config, clock) {
            let reservation = crate::runtime::admission_reservation(
                head,
                config,
                state.used().saturating_add(head.units),
            )
            .ok()
            .or(head.memory_reservation_bytes)
            .or(head.memory_cap_bytes);
            for (name, value) in [
                (
                    "ix_build_head_queued_seconds",
                    Some(clock.saturating_sub(head.enqueued_clock_ms) as f64 / 1000.),
                ),
                (
                    "ix_build_head_memory_reservation_bytes",
                    reservation.map(|bytes| bytes as f64),
                ),
            ] {
                if let Some(value) = value {
                    gauge(
                        name,
                        "Current queue head age or effective memory reservation.",
                        Labels::new(),
                        value,
                    );
                }
            }
        }
        for request in state
            .requests
            .iter()
            .filter(|request| !request.phase.terminal())
        {
            let phase = if request.phase == Phase::Queued {
                "queued"
            } else {
                "running"
            };
            let row = active_memory
                .entry((class(&request.class), phase))
                .or_default();
            row.0 = row.0.saturating_add(request.reservation_bytes());
            row.1 = row.1.saturating_add(request.memory_cap_bytes.unwrap_or(0));
        }
        for ((class, phase), (reserved, limit)) in active_memory {
            for (name, value) in [
                ("ix_build_active_memory_reserved_bytes", reserved),
                ("ix_build_active_memory_limit_bytes", limit),
            ] {
                gauge(
                    name,
                    "Aggregate active request memory by class and lifecycle state.",
                    labels(&[("class", class), ("state", phase)]),
                    value as f64,
                );
            }
        }
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
                "ix_build_oldest_running_seconds",
                running
                    .iter()
                    .filter_map(|r| r.started_clock_ms.map(|start| clock.saturating_sub(start)))
                    .max()
                    .unwrap_or(0) as f64
                    / 1000.,
            ),
            (
                "ix_build_policy_hold_seconds",
                config.hold_ms as f64 / 1000.,
            ),
            ("ix_build_policy_maximum_load", config.maximum_load),
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
            gauge(
                "ix_build_oldest_queued_seconds",
                "Oldest current queue age per class, including saved pre-reboot age.",
                labels(&[("class", priority)]),
                queued
                    .iter()
                    .filter(|r| class(&r.class) == priority)
                    .map(|r| {
                        clock
                            .saturating_sub(r.enqueued_clock_ms)
                            .saturating_add(pre_reboot_wait_ms.get(&r.id.0).copied().unwrap_or(0))
                    })
                    .max()
                    .unwrap_or(0) as f64
                    / 1000.,
            );
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
        let policy_hash = policy_hash(config);
        gauge(
            "ix_build_info",
            "Exporter version and effective policy fingerprint (FNV-1a, not a security digest).",
            labels(&[
                ("version", env!("CARGO_PKG_VERSION")),
                ("commit", crate::build_info::COMMIT),
                ("dirty", crate::build_info::DIRTY),
                ("policy_hash", &policy_hash),
            ]),
            1.,
        );
        for family in &mut families {
            for sample in &mut family.samples {
                sample.labels.insert("host".into(), state.host.clone());
            }
        }
        Ok(families)
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

/// Append a host gauge using the shared exposition spelling.
pub(crate) fn append_gauge(text: &mut String, name: &str, host: &str, value: f64) {
    text.push_str(&format!("# HELP {name} Current exporter observation.\n# TYPE {name} gauge\n{name}{{host=\"{host}\"}} {value}\n"));
}
