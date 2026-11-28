// SPDX-License-Identifier: MIT
//! Durable telemetry and read-only Prometheus exposition.
use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Event, Request, State};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
pub(crate) const BUCKETS_SECONDS: [u64; 7] = [1, 10, 60, 300, 900, 3600, 7200];
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PriorityClass {
    MustFix,
    Point,
    Block,
    Periodic,
    ArchTest,
    Kani,
}

impl PriorityClass {
    pub(crate) const ALL: [Self; 6] = [
        Self::MustFix,
        Self::Point,
        Self::Block,
        Self::Periodic,
        Self::ArchTest,
        Self::Kani,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::MustFix => "must-fix",
            Self::Point => "point",
            Self::Block => "block",
            Self::Periodic => "periodic",
            Self::ArchTest => "arch-test",
            Self::Kani => "kani",
        }
    }

    pub(crate) fn from_class(class: &str) -> Self {
        match class {
            "must-fix" => Self::MustFix,
            "point" => Self::Point,
            "block" => Self::Block,
            "arch-test" => Self::ArchTest,
            "kani" => Self::Kani,
            _ => Self::Periodic,
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

/// Counter sidecar; transitions are folded once before pruning.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub(crate) struct MetricsState {
    #[serde(default)]
    telemetry: crate::telemetry::Ledger,
    #[serde(default)]
    store_id: String,
    #[serde(default)]
    last_event_sequence: u64,
    #[serde(default)]
    pre_reboot_wait_ms: BTreeMap<String, u64>,
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

impl MetricsState {
    pub(crate) fn snapshot_with_policy(
        &self,
        state: &State,
        config: &Config,
        now: u64,
        clock: u64,
    ) -> Result<ix_build::wire::MetricsSnapshot> {
        Ok(ix_build::wire::MetricsSnapshot {
            telemetry: self.telemetry.snapshot(
                state,
                config,
                now,
                clock,
                &self.pre_reboot_wait_ms,
            )?,
        })
    }
    pub(crate) fn has_history(&self) -> bool {
        !self.store_id.is_empty()
    }
    pub(crate) fn can_reconcile(&self, state: &State) -> bool {
        let latest = state.next_event.saturating_sub(1);
        if self.last_event_sequence > latest {
            return false;
        }
        let retained: std::collections::BTreeSet<_> = state
            .events
            .iter()
            .filter_map(|e| event_sequence(&e.event_id))
            .filter(|n| *n > self.last_event_sequence && *n <= latest)
            .collect();
        u64::try_from(retained.len()).ok() == Some(latest - self.last_event_sequence)
    }
    pub(crate) fn exported_sequence(&self) -> u64 {
        self.last_event_sequence
    }
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

    pub(crate) fn allow_legacy_gaps(&mut self, state: &State) -> Result<()> {
        let latest = state.next_event.saturating_sub(1);
        if self.last_event_sequence > latest {
            return Err(Error::new(
                ErrorCode::State,
                "coexistence metrics cursor exceeds state",
            ));
        }
        let retained = state
            .events
            .iter()
            .filter_map(|event| event_sequence(&event.event_id))
            .filter(|sequence| *sequence > self.last_event_sequence && *sequence <= latest)
            .count();
        let missing = latest
            .saturating_sub(self.last_event_sequence)
            .saturating_sub(u64::try_from(retained).unwrap_or(u64::MAX));
        if missing > 0 {
            self.telemetry.legacy_gap(missing)?;
        }
        Ok(())
    }

    pub(crate) fn reconcile_events(&mut self, events: &[Event], next_event: u64) -> Result<()> {
        let mut ordered: Vec<_> = events
            .iter()
            .filter_map(|e| event_sequence(&e.event_id).map(|n| (n, e)))
            .filter(|(n, _)| *n > self.last_event_sequence)
            .collect();
        ordered.sort_by_key(|(n, _)| *n);
        for (sequence, event) in ordered {
            let saved = self
                .pre_reboot_wait_ms
                .get(&event.request_id.0)
                .copied()
                .unwrap_or(0);
            self.telemetry.observe(event, saved)?;
            if event.state == crate::model::Phase::Running || event.state.terminal() {
                self.pre_reboot_wait_ms.remove(&event.request_id.0);
            }
            self.last_event_sequence = sequence;
        }
        self.last_event_sequence = self.last_event_sequence.max(next_event.saturating_sub(1));
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
        now: u64,
        clock: u64,
    ) -> Result<String> {
        let mut output = String::new();
        let host = escape_label(&state.host);
        metric_header(
            &mut output,
            "ix_build_admission_blocked_seconds_total",
            "counter",
            "Host blocked seconds by resource, counted once per host.",
        );
        for reason in BlockReason::ALL {
            let committed = self
                .blocked
                .blocked_millis
                .get(reason.label())
                .copied()
                .unwrap_or(0);
            output.push_str(&format!(
                "ix_build_admission_blocked_seconds_total{{host=\"{host}\",reason=\"{}\"}} {}\n",
                reason.label(),
                seconds(committed.saturating_add(self.active_elapsed(clock, reason)))
            ));
        }
        config.cache.append_info(&mut output, &state.host);
        crate::telemetry::render(
            &self
                .snapshot_with_policy(state, config, now, clock)?
                .telemetry,
            &mut output,
        );
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
    let temporary = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        crate::store::random_id()?
    ));
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Error::new(
            ErrorCode::Configuration,
            "metrics output path is a symlink",
        ));
    }
    if fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return Ok(());
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
pub(crate) mod tests {
    use super::*;
    use crate::config::Config;

    pub(crate) fn policy() -> Result<Config> {
        Ok(serde_json::from_str(include_str!(
            "../policy.example.json"
        ))?)
    }

    pub(crate) fn state(requests: Vec<crate::model::Request>) -> State {
        State {
            min_reader_version: 1,
            schema_version: 1,
            store_id: "0123456789abcdef0123456789abcdef".to_owned(),
            config_path: "/policy.json".into(),
            host: "draco".to_owned(),
            boot_time: 1,
            boot_id: None,
            next_sequence: 100,
            next_event: 1,
            pruned_events: 0,
            metrics: MetricsState::default(),
            requests,
            events: Vec::new(),
        }
    }

    pub(crate) fn request(sequence: u64, owner: String, phase: crate::model::Phase) -> Request {
        Request {
            client: None,
            client_pid_namespace: None,
            telemetry: Default::default(),
            supervision_detail: None,
            trace: Default::default(),
            command_started_clock_ms: None,
            blocked_at_clock_ms: None,
            active_blocked_reasons: Vec::new(),
            measured_memory_bytes: None,
            pid_namespace: None,
            id: crate::model::RequestId(format!("store:r{sequence}")),
            sequence,
            owner,
            ticket: "SWM-242".to_owned(),
            units: 1,
            priority: 0,
            class: "periodic".to_owned(),
            memory_cap_bytes: None,
            memory_reservation_bytes: None,
            memory_limit_key: None,
            memory_cargo_jobs: None,
            memory_rust_test_threads: None,
            memory_parallelism: None,
            caller_cargo_build_jobs: None,
            caller_rust_test_threads: None,
            caller_cargo_incremental: None,
            caller_cargo_target_dir: None,
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
    /// Trace: FR-015-AC-4
    #[test]
    fn exposition_excludes_ticket_request_command_and_environment_values() -> Result<()> {
        let mut request = crate::model::Request {
            client: None,
            client_pid_namespace: None,
            telemetry: Default::default(),
            supervision_detail: None,
            trace: Default::default(),
            command_started_clock_ms: None,
            blocked_at_clock_ms: None,
            active_blocked_reasons: Vec::new(),
            measured_memory_bytes: None,
            pid_namespace: None,
            id: crate::model::RequestId("request-secret-id".to_owned()),
            sequence: 1,
            owner: "draco:ix-relay:build-metrics".to_owned(),
            ticket: "PRIVATE-TICKET".to_owned(),
            units: 1,
            priority: 0,
            class: "periodic".to_owned(),
            memory_cap_bytes: None,
            memory_reservation_bytes: None,
            memory_limit_key: None,
            memory_cargo_jobs: None,
            memory_rust_test_threads: None,
            memory_parallelism: None,
            caller_cargo_build_jobs: None,
            caller_rust_test_threads: None,
            caller_cargo_incremental: None,
            caller_cargo_target_dir: None,
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
        let output = directory.path().join("ix-build.prom");
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

    pub(crate) fn event(sequence: u64, phase: &str, priority: u32, wait_ms: u64) -> Event {
        Event {
            telemetry: Default::default(),
            correction: false,
            trace: Default::default(),
            kind: "transition".to_owned(),
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
            class: match priority {
                0 => "periodic",
                1 => "block",
                2 => "point",
                _ => "must-fix",
            }
            .to_owned(),
            capacity_units: 1,
            free_units: 1,
            wait_ms,
            hold_ms: None,
            reason: None,
        }
    }
}
#[cfg(test)]
#[path = "metrics/telemetry_tests.rs"]
mod telemetry_tests;

#[cfg(test)]
#[path = "metrics/monitoring_tests.rs"]
mod monitoring_tests;
