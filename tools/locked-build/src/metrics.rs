// SPDX-License-Identifier: MIT
//! Durable Prometheus metrics for build outcomes, queue waits and admission.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Event, Request, State};

const BUCKETS_SECONDS: [u64; 12] = [1, 5, 10, 30, 60, 120, 300, 600, 1_200, 1_800, 3_600, 7_200];
const MAX_LANE_LABELS: usize = 64;

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
enum PriorityClass {
    Check,
    Test,
    Gate,
}

impl PriorityClass {
    const ALL: [Self; 3] = [Self::Check, Self::Test, Self::Gate];

    fn label(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Test => "test",
            Self::Gate => "gate",
        }
    }

    fn from_priority(priority: u32) -> Self {
        match priority {
            0 => Self::Check,
            1 => Self::Test,
            _ => Self::Gate,
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
        if event.reason.as_deref() == Some("rss_cap") {
            return Some(Self::RssCap);
        }
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
pub(crate) struct MetricsState {
    /// Store identity to prevent counters from a replaced state directory from being reused.
    #[serde(default)]
    store_id: String,
    /// Outcome counts by closed priority class and outcome name.
    #[serde(default)]
    builds: BTreeMap<String, BTreeMap<String, u64>>,
    /// Queue wait observations by priority, with named bucket bounds in seconds.
    #[serde(default)]
    queue_wait: BTreeMap<String, Histogram>,
    /// Next transition sequence already folded into the counters.
    #[serde(default)]
    last_event_sequence: u64,
    /// Queue age accumulated before reboot, keyed until the request is admitted or ends.
    #[serde(default)]
    pre_reboot_wait_ms: BTreeMap<String, u64>,
    /// One durable host-level accumulator for admission-blocked time.
    #[serde(flatten)]
    blocked: BlockedAccumulator,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct BlockedAccumulator {
    /// Cumulative time by the resource checks currently implemented.
    #[serde(default)]
    blocked_millis: BTreeMap<String, u64>,
    /// Current host-wide denial interval, independent of waiter identity.
    #[serde(default)]
    active_block: Option<(BlockReason, u64)>,
    /// UTC start is retained only to checkpoint an active interval on reboot.
    #[serde(default)]
    active_started_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct Histogram {
    #[serde(default)]
    buckets: BTreeMap<u64, u64>,
    #[serde(default)]
    count: u64,
    #[serde(default)]
    sum_ms: u64,
}

impl MetricsState {
    pub(crate) fn bind_store(&mut self, store_id: &str) -> bool {
        if self.store_id.is_empty() {
            self.store_id = store_id.to_owned();
            true
        } else if self.store_id != store_id {
            *self = Self {
                store_id: store_id.to_owned(),
                ..Self::default()
            };
            true
        } else {
            false
        }
    }

    pub(crate) fn record_pre_reboot_wait(&mut self, request_id: &str, wait_ms: u64) {
        if wait_ms > 0 {
            self.pre_reboot_wait_ms
                .insert(request_id.to_owned(), wait_ms);
        }
    }

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
                let pre_reboot = self
                    .pre_reboot_wait_ms
                    .remove(&event.request_id.0)
                    .unwrap_or(0);
                self.record_queue_wait(event.priority, event.wait_ms.saturating_add(pre_reboot))?;
            } else if let Some(outcome) = Outcome::from_event(event) {
                self.pre_reboot_wait_ms.remove(&event.request_id.0);
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
            .entry(PriorityClass::from_priority(priority).label().to_owned())
            .or_default()
            .entry(outcome.label().to_owned())
            .or_default();
        *count = count
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "build metric counter overflow"))?;
        Ok(())
    }

    fn record_queue_wait(&mut self, priority: u32, wait_ms: u64) -> Result<()> {
        let histogram = self
            .queue_wait
            .entry(PriorityClass::from_priority(priority).label().to_owned())
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
    pub(crate) fn observe_block(
        &mut self,
        reason: Option<&str>,
        now_ms: u64,
        clock_ms: u64,
    ) -> Result<()> {
        let next = reason.and_then(BlockReason::from_admission_reason);
        if self
            .blocked
            .active_block
            .as_ref()
            .is_some_and(|(current, _)| Some(*current) == next)
        {
            return Ok(());
        }
        self.close_block(clock_ms, now_ms)?;
        self.blocked.active_block = next.map(|block_reason| (block_reason, clock_ms));
        self.blocked.active_started_at_ms = next.map(|_| now_ms);
        Ok(())
    }

    /// Persist elapsed blocked time before reboot can reset the monotonic clock.
    pub(crate) fn checkpoint_active(&mut self, clock_ms: u64, now_ms: u64) -> Result<()> {
        if let Some((reason, started)) = self.blocked.active_block {
            let elapsed = clock_ms.saturating_sub(started);
            let total = self
                .blocked
                .blocked_millis
                .entry(reason.label().to_owned())
                .or_default();
            *total = total
                .checked_add(elapsed)
                .ok_or_else(|| Error::new(ErrorCode::State, "admission metric counter overflow"))?;
            self.blocked.active_block = Some((reason, clock_ms));
            self.blocked.active_started_at_ms = Some(now_ms);
        }
        Ok(())
    }

    /// Close the interval when no queued request remains.
    pub(crate) fn close_block(&mut self, clock_ms: u64, now_ms: u64) -> Result<()> {
        self.checkpoint_active(clock_ms, now_ms)?;
        self.blocked.active_block = None;
        self.blocked.active_started_at_ms = None;
        Ok(())
    }

    /// Checkpoint an open interval at the new boot boundary.
    pub(crate) fn finish_for_reboot(
        &mut self,
        new_boot_at_ms: u64,
        last_host_poll_at_ms: Option<u64>,
        poll_ms: u64,
    ) -> Result<()> {
        if let (Some((reason, _)), Some(started)) =
            (self.blocked.active_block, self.blocked.active_started_at_ms)
        {
            let end_at_ms = last_host_poll_at_ms
                .unwrap_or(started)
                .saturating_add(poll_ms.saturating_mul(2))
                .min(new_boot_at_ms);
            let elapsed = end_at_ms.saturating_sub(started);
            let total = self
                .blocked
                .blocked_millis
                .entry(reason.label().to_owned())
                .or_default();
            *total = total
                .checked_add(elapsed)
                .ok_or_else(|| Error::new(ErrorCode::State, "admission metric counter overflow"))?;
        }
        self.blocked.active_block = None;
        self.blocked.active_started_at_ms = None;
        Ok(())
    }

    pub(crate) fn expire_stale_block(
        &mut self,
        requests: &[Request],
        now_ms: u64,
        clock_ms: u64,
        poll_ms: u64,
        mut has_live_waiter: impl FnMut(&Request) -> bool,
    ) -> Result<bool> {
        let Some((_, started_clock_ms)) = self.blocked.active_block else {
            return Ok(false);
        };
        let Some(last_poll_ms) = requests
            .iter()
            .filter(|request| request.phase == crate::model::Phase::Queued)
            .filter_map(|request| request.heartbeat_at_ms)
            .max()
            .or(self.blocked.active_started_at_ms)
        else {
            return Ok(false);
        };
        let grace_ms = poll_ms.saturating_mul(2);
        if now_ms.saturating_sub(last_poll_ms) <= grace_ms {
            return Ok(false);
        }
        if requests
            .iter()
            .filter(|request| request.phase == crate::model::Phase::Queued)
            .any(&mut has_live_waiter)
        {
            return Ok(false);
        }
        let expiry_ms = last_poll_ms.saturating_add(grace_ms);
        let elapsed_after_expiry = now_ms.saturating_sub(expiry_ms);
        let expiry_clock_ms = clock_ms.saturating_sub(elapsed_after_expiry);
        let checkpoint_at_ms =
            expiry_ms.max(self.blocked.active_started_at_ms.unwrap_or(expiry_ms));
        self.checkpoint_active(expiry_clock_ms.max(started_clock_ms), checkpoint_at_ms)?;
        self.blocked.active_block = None;
        self.blocked.active_started_at_ms = None;
        Ok(true)
    }

    pub(crate) fn exposition(
        &self,
        state: &State,
        config: &Config,
        _now_ms: u64,
        clock_ms: u64,
    ) -> Result<String> {
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
            "ix_build_lane_running",
            "gauge",
            "Running build requests by bounded owner lane address.",
        );
        let mut lane_counts = BTreeMap::<String, usize>::new();
        for request in state
            .requests
            .iter()
            .filter(|request| request.phase == crate::model::Phase::Running)
        {
            let lane = if is_lane_address(&request.owner) {
                request.owner.as_str()
            } else {
                "other"
            };
            *lane_counts.entry(lane.to_owned()).or_default() += 1;
        }
        let mut other = 0_usize;
        let mut emitted_lanes = 0_usize;
        for (lane, count) in lane_counts {
            if lane == "other" || emitted_lanes == MAX_LANE_LABELS {
                other += count;
            } else {
                output.push_str(&format!(
                    "ix_build_lane_running{{host=\"{host}\",lane=\"{}\"}} {count}\n",
                    escape_label(&lane)
                ));
                emitted_lanes += 1;
            }
        }
        if other > 0 {
            output.push_str(&format!(
                "ix_build_lane_running{{host=\"{host}\",lane=\"other\"}} {other}\n"
            ));
        }

        metric_header(
            &mut output,
            "ix_build_queue_wait_seconds",
            "histogram",
            "Queue wait duration when a build is admitted.",
        );
        for priority in PriorityClass::ALL {
            let histogram = self.queue_wait.get(priority.label());
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
                    .get(priority.label())
                    .and_then(|counts| counts.get(outcome.label()))
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
            let committed = self
                .blocked
                .blocked_millis
                .get(reason.label())
                .copied()
                .unwrap_or(0);
            let active = self.active_elapsed(clock_ms, reason);
            output.push_str(&format!(
                "ix_build_admission_blocked_seconds_total{{host=\"{host}\",reason=\"{}\"}} {}\n",
                reason.label(),
                seconds(committed.saturating_add(active))
            ));
        }
        Ok(output)
    }

    fn active_elapsed(&self, clock_ms: u64, reason: BlockReason) -> u64 {
        let Some((active_reason, started)) = self.blocked.active_block else {
            return 0;
        };
        if active_reason != reason {
            return 0;
        }
        clock_ms.saturating_sub(started)
    }
}

fn is_lane_address(owner: &str) -> bool {
    let mut segments = owner.split(':');
    matches!(
        (segments.next(), segments.next(), segments.next(), segments.next()),
        (Some(server), Some(workspace), Some(tab), None)
            if !server.is_empty() && !workspace.is_empty() && !tab.is_empty()
    )
}

fn event_sequence(event_id: &str) -> Option<u64> {
    event_id.rsplit_once(":e")?.1.parse().ok()
}

fn metric_header(output: &mut String, name: &str, kind: &str, help: &str) {
    output.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
}

/// Escape a label value for Prometheus text exposition.
pub(crate) fn escape_label(value: &str) -> String {
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

    fn request(sequence: u64, owner: String, phase: crate::model::Phase) -> Request {
        Request {
            id: crate::model::RequestId(format!("store:r{sequence}")),
            sequence,
            owner,
            ticket: "SWM-242".to_owned(),
            units: 1,
            priority: 0,
            memory_cap_bytes: None,
            caller_cargo_build_jobs: None,
            caller_rust_test_threads: None,
            lease_record: None,
            enqueued_at_ms: 0,
            enqueued_clock_ms: 0,
            started_at_ms: None,
            started_clock_ms: None,
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
            command: Vec::new(),
            cwd: "/tmp".into(),
        }
    }

    fn metric_value<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
        text.lines().find_map(|line| line.strip_prefix(prefix))
    }

    fn current_build_fixture() -> Result<String> {
        let requests = vec![
            request(
                1,
                "draco:ix-relay:queued".to_owned(),
                crate::model::Phase::Queued,
            ),
            request(
                2,
                "draco:ix-relay:running".to_owned(),
                crate::model::Phase::Running,
            ),
        ];
        let current = state(requests);
        current.metrics.exposition(&current, &policy()?, 0, 0)
    }

    /// Trace: FR-015-AC-1
    #[test]
    fn exposition_matches_the_current_build_series_matrix() -> Result<()> {
        let text = current_build_fixture()?;
        let names = text
            .lines()
            .filter_map(|line| line.strip_prefix("# TYPE "))
            .filter_map(|line| line.split_once(' '))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "ix_build_queue_depth",
                "ix_build_lane_running",
                "ix_build_queue_wait_seconds",
                "ix_build_slots_busy",
                "ix_build_slots_configured",
                "ix_build_builds_total",
                "ix_build_admission_blocked_seconds_total",
            ]
        );
        assert!(text.contains("ix_build_queue_depth{host=\"draco\",priority=\"check\"} 1"));
        assert!(
            text.contains(
                "ix_build_lane_running{host=\"draco\",lane=\"draco:ix-relay:running\"} 1"
            )
        );
        for absent in [
            "ix_build_slots_max",
            "ix_build_slots_sustainable",
            "ix_build_pause_seconds_total",
            "ix_build_paused_orphans",
            "ix_build_probe_defect",
            "ix_build_agent_processes",
            "ix_build_agent_rss_bytes",
            "ix_build_test_runs_total",
            "ix_build_target_dir_bytes",
            "ix_build_target_dir_slots",
            "ix_build_probe_value",
            "ix_build_probe_unavailable",
        ] {
            assert!(!text.contains(absent), "unexpected series family {absent}");
        }
        for absent_reason in ["swap", "compressor", "operator"] {
            assert!(
                !text.contains(&format!("reason=\"{absent_reason}\"")),
                "unsupported reason {absent_reason} must be absent"
            );
        }
        Ok(())
    }

    /// Trace: FR-015-AC-1
    #[test]
    #[ignore = "external validation lane; set IX_BUILD_PROMTOOL to promtool"]
    fn promtool_accepts_the_current_build_series_fixture() -> Result<()> {
        let promtool = std::env::var_os("IX_BUILD_PROMTOOL")
            .ok_or_else(|| Error::new(ErrorCode::Configuration, "IX_BUILD_PROMTOOL is required"))?;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("metrics.prom");
        fs::write(&path, current_build_fixture()?)?;
        let output = std::process::Command::new(promtool)
            .arg("check")
            .arg("metrics")
            .stdin(std::process::Stdio::from(fs::File::open(&path)?))
            .output()?;
        assert!(
            output.status.success(),
            "promtool check metrics failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    /// Trace: FR-015-AC-6
    #[test]
    fn lane_running_caps_addresses_at_64_and_sums_the_rest_under_other() -> Result<()> {
        let requests = (0..70)
            .map(|index| {
                request(
                    index + 1,
                    format!("draco:ix-relay:lane-{index:02}"),
                    crate::model::Phase::Running,
                )
            })
            .collect();
        let current = state(requests);
        let text = current.metrics.exposition(&current, &policy()?, 0, 0)?;
        let lane_lines = text
            .lines()
            .filter(|line| line.starts_with("ix_build_lane_running{"))
            .collect::<Vec<_>>();
        assert_eq!(lane_lines.len(), 65);
        assert_eq!(
            lane_lines
                .iter()
                .filter(|line| line.contains("lane=\"other\""))
                .count(),
            1
        );
        assert!(
            lane_lines
                .iter()
                .any(|line| { *line == "ix_build_lane_running{host=\"draco\",lane=\"other\"} 6" })
        );
        assert!(lane_lines.iter().all(|line| {
            !line.contains("lane-64")
                && !line.contains("lane-65")
                && !line.contains("lane-66")
                && !line.contains("lane-67")
                && !line.contains("lane-68")
                && !line.contains("lane-69")
        }));
        Ok(())
    }

    /// Trace: FR-015-AC-5
    #[test]
    #[ignore = "named performance benchmark lane; run with --release --ignored"]
    fn exposition_for_10_000_retained_requests_is_under_200ms() -> Result<()> {
        let requests = (0..10_000)
            .map(|index| {
                request(
                    index + 1,
                    format!("draco:ix-relay:lane-{}", index % 70),
                    crate::model::Phase::Queued,
                )
            })
            .collect();
        let current = state(requests);
        let config = policy()?;
        let started = std::time::Instant::now();
        let text = current.metrics.exposition(&current, &config, 0, 0)?;
        let elapsed = started.elapsed();
        eprintln!("10,000-request exposition took {elapsed:?}");
        assert!(text.contains("ix_build_queue_depth{host=\"draco\",priority=\"check\"} 10000"));
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "10,000-request exposition took {elapsed:?}"
        );
        Ok(())
    }

    /// Trace: FR-015-AC-8
    #[test]
    fn closed_outcome_series_start_at_zero_and_only_applicable_outcomes_increment() -> Result<()> {
        let config = policy()?;
        let empty = state(Vec::new());
        let initial = empty.metrics.exposition(&empty, &config, 0, 0)?;
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
        let counted = metrics.exposition(&empty, &config, 0, 0)?;
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
            "ix_build_builds_total{host=\"draco\",priority=\"gate\",outcome=\"timed_out\"}",
        ];
        for key in expected {
            assert_eq!(after.get(key).map(String::as_str), Some("1"));
        }
        assert!(
            changed
                .iter()
                .all(|(key, value)| expected.contains(&key.as_str()) && *value == "1")
        );
        assert!(counted.contains("priority=\"gate\",outcome=\"timed_out\"} 1"));
        assert!(!counted.contains("outcome=\"aborted_thermal\"} 1"));
        Ok(())
    }

    /// Trace: FR-015-AC-9
    #[test]
    fn failed_check_request_increments_only_failed_from_the_zero_start() -> Result<()> {
        let config = policy()?;
        let mut state = state(Vec::new());
        let initial = state.metrics.exposition(&state, &config, 0, 0)?;
        let before = outcome_values(&initial);
        assert!(before.values().all(|value| value == "0"));

        state
            .metrics
            .reconcile_events(&[event(1, "failed", 0, 0)], 2)?;
        let after = outcome_values(&state.metrics.exposition(&state, &config, 0, 0)?);
        let changed = after
            .iter()
            .filter(|(key, value)| before.get(*key) != Some(*value))
            .collect::<Vec<_>>();
        assert_eq!(changed.len(), 1);
        assert_eq!(
            after
                .get("ix_build_builds_total{host=\"draco\",priority=\"check\",outcome=\"failed\"}")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            changed[0].0.as_str(),
            "ix_build_builds_total{host=\"draco\",priority=\"check\",outcome=\"failed\"}"
        );
        Ok(())
    }

    /// Initial zero series are verified here; abort outcome production is outside this slice.
    #[test]
    fn abort_outcome_series_are_exported_at_zero_without_an_abort_producer() -> Result<()> {
        let empty = state(Vec::new());
        let initial = empty.metrics.exposition(&empty, &policy()?, 0, 0)?;
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

    /// Trace: FR-015-AC-11
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
        let text = metrics.exposition(&empty, &config, 0, 0)?;
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

    /// Trace: FR-015-AC-3
    #[test]
    fn queue_wait_histogram_combines_saved_pre_reboot_age_with_monotonic_wait() -> Result<()> {
        let config = policy()?;
        let empty = state(Vec::new());
        let mut metrics = MetricsState::default();
        metrics.record_pre_reboot_wait("store:r1", 4_000);
        metrics.reconcile_events(&[event(1, "running", 0, 80)], 2)?;
        let text = metrics.exposition(&empty, &config, 0, 0)?;
        assert!(
            text.contains(
                "ix_build_queue_wait_seconds_sum{host=\"draco\",priority=\"check\"} 4.080"
            )
        );
        Ok(())
    }

    /// Trace: FR-015-AC-11
    #[test]
    fn blocked_intervals_checkpoint_and_close_when_the_queue_is_empty() -> Result<()> {
        let mut metrics = MetricsState::default();
        metrics.observe_block(Some("memory_reserve"), 10, 100)?;
        metrics.observe_block(Some("memory_reserve"), 20, 350)?;
        metrics.checkpoint_active(600, 20)?;
        metrics.close_block(1_000, 30)?;
        let empty = state(Vec::new());
        let text = metrics.exposition(&empty, &policy()?, 0, 0)?;
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

    /// Trace: FR-015-AC-11
    #[test]
    fn accumulated_block_time_survives_monotonic_clock_reset_after_reboot() -> Result<()> {
        let mut metrics = MetricsState::default();
        metrics.observe_block(Some("memory_reserve"), 1_000, 10)?;
        metrics.finish_for_reboot(3_000, Some(1_900), 100)?;
        let empty = state(Vec::new());
        let text = metrics.exposition(&empty, &policy()?, 0, 0)?;
        assert_eq!(
            metric_value(
                &text,
                "ix_build_admission_blocked_seconds_total{host=\"draco\",reason=\"memory\"} "
            ),
            Some("1.100")
        );
        Ok(())
    }

    /// Trace: FR-015-AC-11
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

    /// Trace: FR-015-AC-11
    #[test]
    fn persisted_host_interval_uses_the_closed_reason_enum() -> Result<()> {
        let persisted = br#"{"active_block":["memory",10],"active_started_at_ms":100}"#;
        let metrics: MetricsState = serde_json::from_slice(persisted)?;
        assert_eq!(metrics.active_elapsed(35, BlockReason::Memory), 25);
        let encoded = serde_json::to_vec(&metrics)?;
        let restored: MetricsState = serde_json::from_slice(&encoded)?;
        assert_eq!(restored.active_elapsed(45, BlockReason::Memory), 35);
        Ok(())
    }

    /// Trace: FR-015-AC-10
    #[test]
    fn unknown_persisted_metric_keys_are_ignored_on_exposition() -> Result<()> {
        let mut metrics: MetricsState = serde_json::from_str(
            r#"{"blocked_millis":{"future_reason":91},"builds":{"future_priority":{"future_outcome":7}},"future_field":true}"#,
        )?;
        let _ = metrics.bind_store("0123456789abcdef0123456789abcdef");
        let empty = state(Vec::new());
        let text = metrics.exposition(&empty, &policy()?, 0, 0)?;
        assert!(!text.contains("future_reason"));
        assert!(!text.contains("future_outcome"));
        assert!(text.contains("reason=\"memory\"} 0.000"));
        Ok(())
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
            memory_cap_bytes: None,
            caller_cargo_build_jobs: None,
            caller_rust_test_threads: None,
            lease_record: None,
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
        let text = state.metrics.exposition(&state, &policy()?, 0, 0)?;
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

    #[test]
    fn cap_kill_reason_is_exported_as_rss_cap_outcome() {
        let mut event = event(1, "failed", 0, 0);
        event.reason = Some("rss_cap".to_owned());

        assert_eq!(Outcome::from_event(&event), Some(Outcome::RssCap));
    }
}
