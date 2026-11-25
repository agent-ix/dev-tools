// SPDX-License-Identifier: MIT
//! Durable Prometheus metrics for build outcomes, queue waits and admission.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Event, State};

const BUCKETS_SECONDS: [u64; 12] = [1, 5, 10, 30, 60, 120, 300, 600, 1_200, 1_800, 3_600, 7_200];

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
enum PriorityClass {
    Check,
    Test,
    Gate,
    Other,
}

impl PriorityClass {
    const ALL: [Self; 4] = [Self::Check, Self::Test, Self::Gate, Self::Other];

    fn label(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Test => "test",
            Self::Gate => "gate",
            Self::Other => "other",
        }
    }

    fn from_priority(priority: u32) -> Self {
        match priority {
            0 => Self::Check,
            1 => Self::Test,
            2 => Self::Gate,
            _ => Self::Other,
        }
    }
}

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Ok,
    Failed,
    Cancelled,
    TimedOut,
    AbortedMemory,
    AbortedSwap,
    AbortedCompressor,
    AbortedHostLoad,
    AbortedThermal,
    RssCap,
    PauseExceeded,
}

impl Outcome {
    const ALL: [Self; 11] = [
        Self::Ok,
        Self::Failed,
        Self::Cancelled,
        Self::TimedOut,
        Self::AbortedMemory,
        Self::AbortedSwap,
        Self::AbortedCompressor,
        Self::AbortedHostLoad,
        Self::AbortedThermal,
        Self::RssCap,
        Self::PauseExceeded,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::AbortedMemory => "aborted_memory",
            Self::AbortedSwap => "aborted_swap",
            Self::AbortedCompressor => "aborted_compressor",
            Self::AbortedHostLoad => "aborted_host_load",
            Self::AbortedThermal => "aborted_thermal",
            Self::RssCap => "rss_cap",
            Self::PauseExceeded => "pause_exceeded",
        }
    }

    fn from_event(event: &Event) -> Option<Self> {
        match event.state {
            crate::model::Phase::Succeeded => Some(Self::Ok),
            crate::model::Phase::Failed => Some(Self::Failed),
            crate::model::Phase::Cancelled => Some(Self::Cancelled),
            crate::model::Phase::TimedOut => Some(Self::TimedOut),
            crate::model::Phase::Queued | crate::model::Phase::Running => None,
        }
    }
}

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
enum BlockReason {
    Memory,
    TargetDisk,
    HostLoad,
    Probe,
}

impl BlockReason {
    const ALL: [Self; 4] = [Self::Memory, Self::TargetDisk, Self::HostLoad, Self::Probe];

    fn label(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::TargetDisk => "target_disk",
            Self::HostLoad => "host_load",
            Self::Probe => "probe",
        }
    }

    fn from_admission_reason(reason: &str) -> Option<Self> {
        match reason {
            "memory_reserve" => Some(Self::Memory),
            "disk_reserve" => Some(Self::TargetDisk),
            "host_load" => Some(Self::HostLoad),
            "host_sample_unavailable" | "host_sample_invalid_or_stale" => Some(Self::Probe),
            _ => None,
        }
    }
}

/// Persisted scheduler counters. Maps make additions backward compatible.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MetricsState {
    /// Outcome counts by closed priority class and outcome name.
    #[serde(default)]
    builds: BTreeMap<PriorityClass, BTreeMap<Outcome, u64>>,
    /// Queue wait observations by priority, with named bucket bounds in seconds.
    #[serde(default)]
    queue_wait: BTreeMap<PriorityClass, Histogram>,
    /// Admission-blocked time by the resource checks currently implemented.
    #[serde(default)]
    blocked_millis: BTreeMap<BlockReason, u64>,
    /// Next transition sequence already folded into the counters.
    #[serde(default)]
    last_event_sequence: u64,
    /// Active interval, checkpointed with every admission poll and metrics scrape.
    #[serde(default)]
    active_block: Option<(BlockReason, u64)>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Histogram {
    #[serde(default)]
    buckets: BTreeMap<u64, u64>,
    #[serde(default)]
    count: u64,
    #[serde(default)]
    sum_ms: u64,
}

impl MetricsState {
    /// Fold new persisted events exactly once into the counter sidecar.
    pub(crate) fn reconcile_events(&mut self, events: &[Event], next_event: u64) -> Result<()> {
        let mut ordered: Vec<_> = events
            .iter()
            .filter_map(|event| event_sequence(&event.event_id).map(|sequence| (sequence, event)))
            .filter(|(sequence, _)| *sequence > self.last_event_sequence)
            .collect();
        ordered.sort_by_key(|(sequence, _)| *sequence);
        for (sequence, event) in ordered {
            if event.state == crate::model::Phase::Running {
                self.record_queue_wait(event.priority, event.wait_ms)?;
            } else if let Some(outcome) = Outcome::from_event(event) {
                self.record_terminal(event.priority, outcome)?;
            }
            self.last_event_sequence = sequence;
        }
        self.last_event_sequence = self.last_event_sequence.max(next_event.saturating_sub(1));
        Ok(())
    }

    fn record_terminal(&mut self, priority: u32, outcome: Outcome) -> Result<()> {
        let count = self
            .builds
            .entry(PriorityClass::from_priority(priority))
            .or_default()
            .entry(outcome)
            .or_default();
        *count = count
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "build metric counter overflow"))?;
        Ok(())
    }

    fn record_queue_wait(&mut self, priority: u32, wait_ms: u64) -> Result<()> {
        let histogram = self
            .queue_wait
            .entry(PriorityClass::from_priority(priority))
            .or_default();
        histogram.count = histogram
            .count
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "queue histogram count overflow"))?;
        histogram.sum_ms = histogram
            .sum_ms
            .checked_add(wait_ms)
            .ok_or_else(|| Error::new(ErrorCode::State, "queue histogram sum overflow"))?;
        for bound in BUCKETS_SECONDS {
            if wait_ms <= bound.saturating_mul(1_000) {
                let count = histogram.buckets.entry(bound).or_default();
                *count = count.checked_add(1).ok_or_else(|| {
                    Error::new(ErrorCode::State, "queue histogram bucket overflow")
                })?;
            }
        }
        Ok(())
    }

    /// Checkpoint one host interval. The state-file lock serializes all waiters.
    pub(crate) fn observe_block(&mut self, reason: Option<&str>, clock_ms: u64) -> Result<()> {
        let next = reason.and_then(BlockReason::from_admission_reason);
        self.checkpoint_active(clock_ms)?;
        self.active_block = next.map(|block_reason| (block_reason, clock_ms));
        Ok(())
    }

    /// Persist elapsed blocked time before reboot can reset the monotonic clock.
    pub(crate) fn checkpoint_active(&mut self, clock_ms: u64) -> Result<()> {
        if let Some((reason, started)) = self.active_block {
            let elapsed = clock_ms.saturating_sub(started);
            let total = self.blocked_millis.entry(reason).or_default();
            *total = total
                .checked_add(elapsed)
                .ok_or_else(|| Error::new(ErrorCode::State, "admission metric counter overflow"))?;
            self.active_block = Some((reason, clock_ms));
        }
        Ok(())
    }

    /// Close the interval when no queued request remains.
    pub(crate) fn close_block(&mut self, clock_ms: u64) -> Result<()> {
        self.checkpoint_active(clock_ms)?;
        self.active_block = None;
        Ok(())
    }

    /// Discard only the uncheckpointed monotonic segment after an OS reboot.
    pub(crate) fn reset_after_reboot(&mut self) {
        self.active_block = None;
    }

    pub(crate) fn exposition(&self, state: &State, config: &Config) -> Result<String> {
        let mut output = String::new();
        let host = escape_label(&state.host);
        metric_header(
            &mut output,
            "ix_build_queue_depth",
            "gauge",
            "Queued build requests by priority class.",
        );
        for priority in PriorityClass::ALL {
            let depth = state
                .requests
                .iter()
                .filter(|request| {
                    request.phase == crate::model::Phase::Queued
                        && PriorityClass::from_priority(request.priority) == priority
                })
                .count();
            output.push_str(&format!(
                "ix_build_queue_depth{{host=\"{host}\",priority=\"{}\"}} {depth}\n",
                priority.label()
            ));
        }

        metric_header(
            &mut output,
            "ix_build_queue_wait_seconds",
            "histogram",
            "Queue wait duration when a build is admitted.",
        );
        for priority in PriorityClass::ALL {
            let histogram = self.queue_wait.get(&priority);
            let count = histogram.map_or(0, |value| value.count);
            for bound in BUCKETS_SECONDS {
                let bucket = histogram
                    .and_then(|value| value.buckets.get(&bound))
                    .copied()
                    .unwrap_or(0);
                output.push_str(&format!("ix_build_queue_wait_seconds_bucket{{host=\"{host}\",priority=\"{}\",le=\"{bound}\"}} {bucket}\n", priority.label()));
            }
            output.push_str(&format!("ix_build_queue_wait_seconds_bucket{{host=\"{host}\",priority=\"{}\",le=\"+Inf\"}} {count}\n", priority.label()));
            let sum_ms = histogram.map_or(0, |value| value.sum_ms);
            output.push_str(&format!(
                "ix_build_queue_wait_seconds_sum{{host=\"{host}\",priority=\"{}\"}} {}\n",
                priority.label(),
                seconds(sum_ms)
            ));
            output.push_str(&format!(
                "ix_build_queue_wait_seconds_count{{host=\"{host}\",priority=\"{}\"}} {count}\n",
                priority.label()
            ));
        }

        metric_header(
            &mut output,
            "ix_build_slots_busy",
            "gauge",
            "Currently occupied build units.",
        );
        output.push_str(&format!(
            "ix_build_slots_busy{{host=\"{host}\"}} {}\n",
            state.used()
        ));
        metric_header(
            &mut output,
            "ix_build_slots_configured",
            "gauge",
            "Configured build admission units from locked-build JSON host policy capacity_units until the SWM-239 TOML profile exists; no sustainable capacity is derived.",
        );
        output.push_str(&format!(
            "ix_build_slots_configured{{host=\"{host}\"}} {}\n",
            config.capacity_units
        ));

        metric_header(
            &mut output,
            "ix_build_builds_total",
            "counter",
            "Build requests by priority class and terminal outcome.",
        );
        for priority in PriorityClass::ALL {
            for outcome in Outcome::ALL {
                let value = self
                    .builds
                    .get(&priority)
                    .and_then(|counts| counts.get(&outcome))
                    .copied()
                    .unwrap_or(0);
                output.push_str(&format!("ix_build_builds_total{{host=\"{host}\",priority=\"{}\",outcome=\"{}\"}} {value}\n", priority.label(), outcome.label()));
            }
        }

        metric_header(
            &mut output,
            "ix_build_admission_blocked_seconds_total",
            "counter",
            "Host seconds blocked by measured admission resource limits, counted once per host.",
        );
        for reason in BlockReason::ALL {
            let value = self.blocked_millis.get(&reason).copied().unwrap_or(0);
            output.push_str(&format!(
                "ix_build_admission_blocked_seconds_total{{host=\"{host}\",reason=\"{}\"}} {}\n",
                reason.label(),
                seconds(value)
            ));
        }
        Ok(output)
    }
}

fn event_sequence(event_id: &str) -> Option<u64> {
    event_id.rsplit_once(":e")?.1.parse().ok()
}

fn metric_header(output: &mut String, name: &str, kind: &str, help: &str) {
    output.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn seconds(milliseconds: u64) -> String {
    format!("{}.{:03}", milliseconds / 1_000, milliseconds % 1_000)
}

/// Atomically replace a node-exporter textfile metric document.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| {
            Error::new(
                ErrorCode::Configuration,
                "metrics output path has no file name",
            )
        })?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Error::new(
            ErrorCode::Configuration,
            "metrics output path is a symlink",
        ));
    }
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        OpenOptions::new().read(true).open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn policy() -> Result<Config> {
        Ok(serde_json::from_str(include_str!(
            "../policy.example.json"
        ))?)
    }

    fn state(requests: Vec<crate::model::Request>) -> State {
        State {
            schema_version: 1,
            store_id: "0123456789abcdef0123456789abcdef".to_owned(),
            config_path: "/policy.json".into(),
            host: "draco".to_owned(),
            boot_time: 1,
            next_sequence: 100,
            next_event: 1,
            pruned_events: 0,
            metrics: MetricsState::default(),
            requests,
            events: Vec::new(),
        }
    }

    fn metric_value<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
        text.lines().find_map(|line| line.strip_prefix(prefix))
    }

    /// Trace: FR-015-AC-8
    #[test]
    fn closed_outcome_series_start_at_zero_and_only_applicable_outcomes_increment() -> Result<()> {
        let config = policy()?;
        let empty = state(Vec::new());
        let initial = empty.metrics.exposition(&empty, &config)?;
        for priority in PriorityClass::ALL {
            for outcome in Outcome::ALL {
                assert!(initial.contains(&format!(
                    "ix_build_builds_total{{host=\"draco\",priority=\"{}\",outcome=\"{}\"}} 0",
                    priority.label(),
                    outcome.label()
                )));
            }
        }

        let mut metrics = MetricsState::default();
        let events = [
            event(1, "succeeded", 0, 1_500),
            event(2, "failed", 1, 3_000),
            event(3, "cancelled", 2, 0),
            event(4, "timed_out", 3, 0),
        ];
        metrics.reconcile_events(&events, 5)?;
        let counted = metrics.exposition(&empty, &config)?;
        let before = outcome_values(&initial);
        let after = outcome_values(&counted);
        assert_eq!(before.len(), after.len());
        let changed = after
            .iter()
            .filter(|(key, value)| before.get(*key) != Some(*value))
            .collect::<Vec<_>>();
        assert_eq!(changed.len(), 4);
        let expected = [
            "ix_build_builds_total{host=\"draco\",priority=\"check\",outcome=\"ok\"}",
            "ix_build_builds_total{host=\"draco\",priority=\"test\",outcome=\"failed\"}",
            "ix_build_builds_total{host=\"draco\",priority=\"gate\",outcome=\"cancelled\"}",
            "ix_build_builds_total{host=\"draco\",priority=\"other\",outcome=\"timed_out\"}",
        ];
        for key in expected {
            assert_eq!(after.get(key).map(String::as_str), Some("1"));
        }
        assert!(
            changed
                .iter()
                .all(|(key, value)| expected.contains(&key.as_str()) && *value == "1")
        );
        assert!(counted.contains("priority=\"other\",outcome=\"timed_out\"} 1"));
        assert!(!counted.contains("outcome=\"aborted_thermal\"} 1"));
        Ok(())
    }

    /// Initial zero series are verified here; abort outcome production is outside this slice.
    #[test]
    fn abort_outcome_series_are_exported_at_zero_without_an_abort_producer() -> Result<()> {
        let empty = state(Vec::new());
        let initial = empty.metrics.exposition(&empty, &policy()?)?;
        for priority in PriorityClass::ALL {
            for outcome in [
                Outcome::AbortedMemory,
                Outcome::AbortedSwap,
                Outcome::AbortedCompressor,
                Outcome::AbortedHostLoad,
                Outcome::AbortedThermal,
            ] {
                assert!(initial.contains(&format!(
                    "ix_build_builds_total{{host=\"draco\",priority=\"{}\",outcome=\"{}\"}} 0",
                    priority.label(),
                    outcome.label()
                )));
            }
        }
        Ok(())
    }

    /// Trace: FR-015-AC-1
    #[test]
    fn event_reconciliation_records_wait_histograms_and_outcomes_once() -> Result<()> {
        let config = policy()?;
        let empty = state(Vec::new());
        let mut metrics = MetricsState::default();
        let events = [
            event(1, "running", 0, 1_500),
            event(2, "succeeded", 0, 1_500),
        ];
        metrics.reconcile_events(&events, 3)?;
        metrics.reconcile_events(&events, 3)?;
        let text = metrics.exposition(&empty, &config)?;
        assert!(text.contains("# TYPE ix_build_queue_depth gauge"));
        assert!(text.contains("# TYPE ix_build_builds_total counter"));
        assert!(text.contains("# TYPE ix_build_queue_wait_seconds histogram"));
        assert!(
            text.contains("ix_build_queue_wait_seconds_count{host=\"draco\",priority=\"check\"} 1")
        );
        assert!(text.contains(
            "ix_build_queue_wait_seconds_bucket{host=\"draco\",priority=\"check\",le=\"5\"} 1"
        ));
        assert!(text.contains("outcome=\"ok\"} 1"));
        Ok(())
    }

    /// Trace: FR-015-AC-1
    #[test]
    fn blocked_intervals_checkpoint_and_close_when_the_queue_is_empty() -> Result<()> {
        let mut metrics = MetricsState::default();
        metrics.observe_block(Some("memory_reserve"), 100)?;
        metrics.observe_block(Some("memory_reserve"), 350)?;
        metrics.checkpoint_active(600)?;
        metrics.close_block(1_000)?;
        let empty = state(Vec::new());
        let text = metrics.exposition(&empty, &policy()?)?;
        assert_eq!(
            metric_value(
                &text,
                "ix_build_admission_blocked_seconds_total{host=\"draco\",reason=\"memory\"} "
            ),
            Some("0.900")
        );
        assert!(!text.contains("reason=\"operator\""));
        assert!(!text.contains("reason=\"swap\""));
        assert!(!text.contains("reason=\"compressor\""));
        Ok(())
    }

    #[test]
    fn accumulated_block_time_survives_monotonic_clock_reset_after_reboot() -> Result<()> {
        let mut metrics = MetricsState::default();
        metrics.observe_block(Some("memory_reserve"), 10)?;
        metrics.checkpoint_active(20)?;
        metrics.reset_after_reboot();
        metrics.checkpoint_active(2)?;
        let empty = state(Vec::new());
        let text = metrics.exposition(&empty, &policy()?)?;
        assert_eq!(
            metric_value(
                &text,
                "ix_build_admission_blocked_seconds_total{host=\"draco\",reason=\"memory\"} "
            ),
            Some("0.010")
        );
        Ok(())
    }

    #[test]
    fn blocked_reason_mapping_is_closed_to_current_resource_checks() {
        for (source, expected) in [
            ("memory_reserve", Some(BlockReason::Memory)),
            ("disk_reserve", Some(BlockReason::TargetDisk)),
            ("host_load", Some(BlockReason::HostLoad)),
            ("host_sample_unavailable", Some(BlockReason::Probe)),
            ("host_sample_invalid_or_stale", Some(BlockReason::Probe)),
            ("capacity_shrunk", None),
            ("capacity_occupied", None),
            ("fifo_or_priority_reservation", None),
            ("operator", None),
            ("swap", None),
            ("compressor", None),
        ] {
            assert_eq!(
                BlockReason::from_admission_reason(source),
                expected,
                "{source}"
            );
        }
    }

    /// Trace: FR-015-AC-4
    #[test]
    fn exposition_excludes_ticket_request_command_and_environment_values() -> Result<()> {
        let mut request = crate::model::Request {
            id: crate::model::RequestId("request-secret-id".to_owned()),
            sequence: 1,
            owner: "draco:ix-relay:build-metrics".to_owned(),
            ticket: "PRIVATE-TICKET".to_owned(),
            units: 1,
            priority: 0,
            enqueued_at_ms: 100,
            enqueued_clock_ms: 100,
            started_at_ms: None,
            started_clock_ms: None,
            ended_at_ms: None,
            ended_clock_ms: None,
            heartbeat_at_ms: None,
            worker: None,
            worker_generation: 0,
            watchdog: None,
            command_finished: None,
            phase: crate::model::Phase::Queued,
            stop: None,
            exit_code: None,
            signal: None,
            reason: Some("private-reason".to_owned()),
            command: vec!["private-command".to_owned()],
            cwd: "/private".into(),
        };
        request.cwd = "/PRIVATE-WORKDIR".into();
        let state = state(vec![request]);
        let text = state.metrics.exposition(&state, &policy()?)?;
        for secret in [
            "request-secret-id",
            "PRIVATE-TICKET",
            "private-command",
            "private-reason",
            "PRIVATE-WORKDIR",
        ] {
            assert!(!text.contains(secret));
        }
        Ok(())
    }

    #[test]
    fn textfile_export_is_readable_and_replaces_destination() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let output = directory.path().join("locked-build.prom");
        write_atomic(&output, "# TYPE example gauge\nexample 1\n")?;
        assert_eq!(
            fs::read_to_string(&output)?,
            "# TYPE example gauge\nexample 1\n"
        );
        assert_eq!(fs::metadata(output)?.permissions().mode() & 0o777, 0o644);
        Ok(())
    }

    #[test]
    fn failed_textfile_export_removes_temporary_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let output = directory.path().join("collector.prom");
        fs::create_dir(&output)?;
        assert!(write_atomic(&output, "metric 1\n").is_err());
        let leftovers = fs::read_dir(directory.path())?
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        Ok(())
    }

    fn outcome_values(text: &str) -> BTreeMap<String, String> {
        text.lines()
            .filter(|line| line.starts_with("ix_build_builds_total{"))
            .filter_map(|line| {
                line.split_once(' ')
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
            })
            .collect()
    }

    fn event(sequence: u64, phase: &str, priority: u32, wait_ms: u64) -> Event {
        Event {
            schema_version: 1,
            event_id: format!("store:e{sequence}"),
            request_id: crate::model::RequestId(format!("store:r{sequence}")),
            host: "draco".to_owned(),
            owner: "draco:ix-relay:build-metrics".to_owned(),
            ticket: "SWM-242".to_owned(),
            observed_at_ms: sequence,
            state: match phase {
                "queued" => crate::model::Phase::Queued,
                "running" => crate::model::Phase::Running,
                "succeeded" => crate::model::Phase::Succeeded,
                "failed" => crate::model::Phase::Failed,
                "cancelled" => crate::model::Phase::Cancelled,
                "timed_out" => crate::model::Phase::TimedOut,
                _ => unreachable!("test phase is closed"),
            },
            units: 1,
            priority,
            capacity_units: 1,
            free_units: 1,
            wait_ms,
            hold_ms: None,
            reason: None,
        }
    }
}
