// SPDX-License-Identifier: MIT
//! Durable Prometheus metrics for build outcomes, queue waits and admission.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Phase, Request, State};

const PRIORITIES: [&str; 3] = ["check", "test", "gate"];
const BUCKETS_SECONDS: [u64; 12] = [1, 5, 10, 30, 60, 120, 300, 600, 1_200, 1_800, 3_600, 7_200];
const OUTCOMES: [Outcome; 11] = [
    Outcome::Ok,
    Outcome::Failed,
    Outcome::Cancelled,
    Outcome::TimedOut,
    Outcome::AbortedMemory,
    Outcome::AbortedSwap,
    Outcome::AbortedCompressor,
    Outcome::AbortedHostLoad,
    Outcome::AbortedThermal,
    Outcome::RssCap,
    Outcome::PauseExceeded,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
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

    fn for_request(request: &Request) -> Self {
        match request.reason.as_deref() {
            Some("aborted_memory") => Self::AbortedMemory,
            Some("aborted_swap") => Self::AbortedSwap,
            Some("aborted_compressor") => Self::AbortedCompressor,
            Some("aborted_host_load") => Self::AbortedHostLoad,
            Some("aborted_thermal") => Self::AbortedThermal,
            Some("rss_cap") => Self::RssCap,
            Some("pause_exceeded") => Self::PauseExceeded,
            _ => match request.phase {
                Phase::Succeeded => Self::Ok,
                Phase::Failed => Self::Failed,
                Phase::Cancelled => Self::Cancelled,
                Phase::TimedOut => Self::TimedOut,
                Phase::Queued | Phase::Running => Self::Failed,
            },
        }
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MetricsState {
    /// Persisted once-only counts indexed by check/test/gate and the closed outcome set.
    pub(crate) builds: [[u64; 11]; 3],
    /// Cumulative bucket observations, count and sum, indexed by priority class.
    pub(crate) queue_wait: [Histogram; 3],
    /// Host-wide admission-blocked milliseconds. This is shared by all waiters.
    pub(crate) blocked_millis: [u64; 5],
    /// Current interval, accounted once under the serialized state lock.
    pub(crate) active_block: Option<(usize, u64)>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Histogram {
    pub(crate) buckets: [u64; 12],
    pub(crate) count: u64,
    pub(crate) sum_ms: u64,
}

impl MetricsState {
    fn priority_index(priority: u32) -> Result<usize> {
        usize::try_from(priority)
            .ok()
            .filter(|index| *index < PRIORITIES.len())
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::Configuration,
                    "metrics require check/test/gate priorities",
                )
            })
    }

    pub(crate) fn record_terminal(&mut self, request: &Request) -> Result<()> {
        let priority = Self::priority_index(request.priority)?;
        let outcome = Outcome::for_request(request);
        let counter = self
            .builds
            .get_mut(priority)
            .and_then(|counts| counts.get_mut(OUTCOMES.iter().position(|item| *item == outcome)?))
            .ok_or_else(|| Error::new(ErrorCode::State, "build metric counter unavailable"))?;
        *counter = counter
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "build metric counter overflow"))?;
        Ok(())
    }

    pub(crate) fn record_queue_wait(&mut self, priority: u32, wait_ms: u64) -> Result<()> {
        let index = Self::priority_index(priority)?;
        let histogram = self
            .queue_wait
            .get_mut(index)
            .ok_or_else(|| Error::new(ErrorCode::State, "queue histogram unavailable"))?;
        histogram.count = histogram
            .count
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "queue histogram count overflow"))?;
        histogram.sum_ms = histogram
            .sum_ms
            .checked_add(wait_ms)
            .ok_or_else(|| Error::new(ErrorCode::State, "queue histogram sum overflow"))?;
        for (count, bound) in histogram.buckets.iter_mut().zip(BUCKETS_SECONDS) {
            if wait_ms <= bound.saturating_mul(1_000) {
                *count = count.checked_add(1).ok_or_else(|| {
                    Error::new(ErrorCode::State, "queue histogram bucket overflow")
                })?;
            }
        }
        Ok(())
    }

    pub(crate) fn observe_block(&mut self, reason: Option<&str>, clock_ms: u64) -> Result<()> {
        let index = match reason {
            Some("memory_reserve") => Some(0),
            Some("disk_reserve") => Some(1),
            Some("host_load") => Some(2),
            Some("host_sample_unavailable" | "host_sample_invalid_or_stale") => Some(3),
            Some("operator") => Some(4),
            _ => None,
        };
        if let Some((previous_index, previous_clock)) = self.active_block {
            let duration_ms = clock_ms.saturating_sub(previous_clock);
            let counter = self.blocked_millis.get_mut(previous_index).ok_or_else(|| {
                Error::new(ErrorCode::State, "admission metric counter unavailable")
            })?;
            *counter = counter
                .checked_add(duration_ms)
                .ok_or_else(|| Error::new(ErrorCode::State, "admission metric counter overflow"))?;
        }
        self.active_block = index.map(|block_index| (block_index, clock_ms));
        Ok(())
    }

    pub(crate) fn exposition(
        &self,
        state: &State,
        config: &Config,
        now_clock_ms: u64,
    ) -> Result<String> {
        let mut output = String::new();
        let host = escape_label(&state.host);
        metric_header(
            &mut output,
            "ix_build_queue_depth",
            "Gauge",
            "Queued build requests by priority class.",
        );
        for (index, priority) in PRIORITIES.iter().enumerate() {
            let depth = state
                .requests
                .iter()
                .filter(|request| {
                    request.phase == Phase::Queued
                        && usize::try_from(request.priority).ok() == Some(index)
                })
                .count();
            output.push_str(&format!(
                "ix_build_queue_depth{{host=\"{host}\",priority=\"{priority}\"}} {depth}\n"
            ));
        }
        metric_header(
            &mut output,
            "ix_build_queue_wait_seconds",
            "Histogram",
            "Queue wait duration when a build is admitted.",
        );
        for (index, priority) in PRIORITIES.iter().enumerate() {
            let histogram = self
                .queue_wait
                .get(index)
                .ok_or_else(|| Error::new(ErrorCode::State, "queue histogram unavailable"))?;
            for (bucket_count, bound) in histogram.buckets.iter().zip(BUCKETS_SECONDS) {
                output.push_str(&format!("ix_build_queue_wait_seconds_bucket{{host=\"{host}\",priority=\"{priority}\",le=\"{bound}\"}} {bucket_count}\n"));
            }
            output.push_str(&format!("ix_build_queue_wait_seconds_bucket{{host=\"{host}\",priority=\"{priority}\",le=\"+Inf\"}} {}\n", histogram.count));
            output.push_str(&format!(
                "ix_build_queue_wait_seconds_sum{{host=\"{host}\",priority=\"{priority}\"}} {}\n",
                seconds(histogram.sum_ms)
            ));
            output.push_str(&format!(
                "ix_build_queue_wait_seconds_count{{host=\"{host}\",priority=\"{priority}\"}} {}\n",
                histogram.count
            ));
        }
        metric_header(
            &mut output,
            "ix_build_slots_busy",
            "Gauge",
            "Currently occupied build units.",
        );
        output.push_str(&format!(
            "ix_build_slots_busy{{host=\"{host}\"}} {}\n",
            state.used()
        ));
        metric_header(
            &mut output,
            "ix_build_slots_configured",
            "Gauge",
            "Currently configured build admission units from host policy capacity_units.",
        );
        output.push_str(&format!(
            "ix_build_slots_configured{{host=\"{host}\"}} {}\n",
            config.capacity_units
        ));
        metric_header(
            &mut output,
            "ix_build_builds_total",
            "Counter",
            "Build requests by priority class and terminal outcome.",
        );
        for (priority_index, priority) in PRIORITIES.iter().enumerate() {
            for (outcome_index, outcome) in OUTCOMES.iter().enumerate() {
                let value = self
                    .builds
                    .get(priority_index)
                    .and_then(|row| row.get(outcome_index))
                    .ok_or_else(|| {
                        Error::new(ErrorCode::State, "build metric counter unavailable")
                    })?;
                output.push_str(&format!("ix_build_builds_total{{host=\"{host}\",priority=\"{priority}\",outcome=\"{}\"}} {value}\n", outcome.label()));
            }
        }
        metric_header(
            &mut output,
            "ix_build_admission_blocked_seconds_total",
            "Counter",
            "Host seconds blocked by admission resource limits, counted once per host.",
        );
        let labels = ["memory", "target_disk", "host_load", "probe", "operator"];
        let mut blocked = self.blocked_millis;
        if let Some((index, start)) = self.active_block {
            if let Some(value) = blocked.get_mut(index) {
                *value = value.saturating_add(now_clock_ms.saturating_sub(start));
            }
        }
        for (label, value) in labels.iter().zip(blocked) {
            output.push_str(&format!("ix_build_admission_blocked_seconds_total{{host=\"{host}\",reason=\"{label}\"}} {}\n", seconds(value)));
        }
        Ok(output)
    }
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
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o644)
        .open(&temporary)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    OpenOptions::new().read(true).open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::{Completion, RequestId};

    fn policy() -> Config {
        serde_json::from_str(include_str!("../policy.example.json")).expect("valid example policy")
    }

    fn request(sequence: u64, priority: u32, phase: Phase) -> Request {
        Request {
            id: RequestId(format!("store:r{sequence}")),
            sequence,
            owner: "draco:ix-relay:build-metrics".to_owned(),
            ticket: "SWM-242".to_owned(),
            units: 1,
            priority,
            enqueued_at_ms: 100,
            enqueued_clock_ms: 100,
            started_at_ms: (phase == Phase::Running).then_some(200),
            started_clock_ms: (phase == Phase::Running).then_some(200),
            ended_at_ms: None,
            ended_clock_ms: None,
            heartbeat_at_ms: None,
            worker: None,
            worker_generation: 0,
            watchdog: None,
            command_finished: None,
            phase,
            stop: None,
            exit_code: None,
            signal: None,
            reason: None,
            command: vec!["true".to_owned()],
            cwd: "/".into(),
        }
    }

    fn state(requests: Vec<Request>) -> State {
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

    /// Trace: FR-015-AC-3, FR-015-AC-8, FR-015-AC-9, FR-015-AC-10
    #[test]
    fn outcome_counts_and_wait_histogram_survive_serialization_and_pruning() -> Result<()> {
        let config = policy();
        let empty = state(Vec::new());
        let initial = empty.metrics.exposition(&empty, &config, 100)?;
        for priority in PRIORITIES {
            for outcome in OUTCOMES {
                assert!(initial.contains(&format!(
                    "ix_build_builds_total{{host=\"draco\",priority=\"{priority}\",outcome=\"{}\"}} 0",
                    outcome.label()
                )));
            }
        }
        let mut state = state(vec![
            request(1, 0, Phase::Running),
            request(2, 1, Phase::Running),
            request(3, 2, Phase::Queued),
            request(4, 0, Phase::Running),
            request(5, 1, Phase::Running),
        ]);
        state.finish(
            &RequestId("store:r1".to_owned()),
            Completion {
                code: Some(0),
                ..Completion::default()
            },
            &config,
            300,
            300,
        )?;
        state.finish(
            &RequestId("store:r1".to_owned()),
            Completion {
                code: Some(0),
                ..Completion::default()
            },
            &config,
            400,
            400,
        )?;
        state.finish(
            &RequestId("store:r2".to_owned()),
            Completion {
                code: Some(7),
                ..Completion::default()
            },
            &config,
            300,
            300,
        )?;
        state.requests[2].stop = Some(crate::model::StopReason::Cancelled);
        state.finish(
            &RequestId("store:r3".to_owned()),
            Completion::default(),
            &config,
            300,
            300,
        )?;
        state.requests[3].stop = Some(crate::model::StopReason::TimedOut);
        state.finish(
            &RequestId("store:r4".to_owned()),
            Completion {
                code: Some(124),
                ..Completion::default()
            },
            &config,
            300,
            300,
        )?;
        state.requests[4].stop = Some(crate::model::StopReason::Cancelled);
        state.finish(
            &RequestId("store:r5".to_owned()),
            Completion::default(),
            &config,
            300,
            300,
        )?;
        state.metrics.record_queue_wait(0, 1_500)?;
        state.metrics.record_queue_wait(1, 3_000)?;

        let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state)?)?;
        restored
            .requests
            .retain(|request| !request.phase.terminal());
        restored.events.clear();
        let text = restored.metrics.exposition(&restored, &config, 500)?;
        assert!(
            text.contains(
                "ix_build_builds_total{host=\"draco\",priority=\"check\",outcome=\"ok\"} 1"
            )
        );
        assert!(text.contains(
            "ix_build_builds_total{host=\"draco\",priority=\"test\",outcome=\"failed\"} 1"
        ));
        assert!(text.contains(
            "ix_build_builds_total{host=\"draco\",priority=\"gate\",outcome=\"cancelled\"} 1"
        ));
        assert!(text.contains(
            "ix_build_builds_total{host=\"draco\",priority=\"check\",outcome=\"timed_out\"} 1"
        ));
        assert!(text.contains(
            "ix_build_builds_total{host=\"draco\",priority=\"test\",outcome=\"cancelled\"} 1"
        ));
        assert!(text.contains(
            "ix_build_builds_total{host=\"draco\",priority=\"check\",outcome=\"aborted_memory\"} 0"
        ));
        assert!(
            text.contains("ix_build_queue_wait_seconds_count{host=\"draco\",priority=\"check\"} 1")
        );
        assert!(
            text.contains(
                "ix_build_queue_wait_seconds_sum{host=\"draco\",priority=\"check\"} 1.500"
            )
        );
        assert!(text.contains("ix_build_slots_configured{host=\"draco\"} 0"));
        assert!(!text.contains("ix_build_pause_seconds_total"));
        Ok(())
    }

    /// Trace: FR-015-AC-1
    #[test]
    fn admission_blocked_time_is_accounted_once_for_the_host() -> Result<()> {
        let mut metrics = MetricsState::default();
        metrics.observe_block(Some("memory_reserve"), 100)?;
        metrics.observe_block(Some("memory_reserve"), 350)?;
        metrics.observe_block(Some("memory_reserve"), 600)?;
        let state = state(Vec::new());
        let text = metrics.exposition(&state, &policy(), 1_000)?;
        assert!(text.contains(
            "ix_build_admission_blocked_seconds_total{host=\"draco\",reason=\"memory\"} 0.900"
        ));
        Ok(())
    }

    #[test]
    fn textfile_export_replaces_the_destination_with_complete_metrics() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let output = directory.path().join("locked-build.prom");
        write_atomic(&output, "# TYPE example gauge\nexample 1\n")?;
        assert_eq!(
            fs::read_to_string(output)?,
            "# TYPE example gauge\nexample 1\n"
        );
        Ok(())
    }
}
