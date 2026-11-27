// SPDX-License-Identifier: MIT
//! One diagnostic object feeds stderr, the durable ledger and status.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use locked_build::wire::{Code, Diagnostic, Value};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{MemoryEnforcement, Phase, Request, StopReason};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Failure {
    pub request_id: String,
    pub class: String,
    #[serde(flatten)]
    pub diagnostic: Diagnostic,
}

impl Failure {
    pub(crate) fn refused(
        code: Code,
        class: &str,
        key: &str,
        limit: Value,
        measured: Value,
        what: &str,
    ) -> Self {
        Self {
            request_id: "pending".to_owned(),
            class: class.to_owned(),
            diagnostic: Diagnostic::failure(
                code,
                "pending",
                key,
                limit,
                measured,
                what,
                "not-started/command",
            ),
        }
    }

    pub(crate) fn emit(&self, state: &Path) -> Result<()> {
        if let Some(message) = &self.diagnostic.message {
            eprintln!("{message}");
        }
        // The same typed payload is durable even when admission was refused
        // before a request could occupy the queue.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .mode(0o600)
            .open(state.join("failures.jsonl"))?;
        file.lock()?;
        serde_json::to_writer(&mut file, self)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }
}

fn command_name(request: &Request) -> String {
    request
        .command
        .first()
        .and_then(|command| Path::new(command).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "command".to_owned())
}

pub(crate) fn process(request: &Request, refused: bool) -> String {
    let name = command_name(request);
    if refused {
        return format!("not-started/{name}");
    }
    request.watchdog.as_ref().map_or_else(
        || format!("unknown/{name}"),
        |watchdog| format!("pgid:{}/{name}", watchdog.pid),
    )
}

pub(crate) fn for_request(request: &Request, config: &Config, clock: u64) -> Diagnostic {
    let id = &request.id.0;
    if request.reason.as_deref() == Some("operator_release") {
        return Diagnostic::failure(
            Code::SupervisionLost,
            id,
            "policy.operator_release_allowed",
            true.into(),
            true.into(),
            request
                .supervision_detail
                .as_deref()
                .unwrap_or("explicit operator release"),
            "not-signalled/operator-release",
        );
    }
    let memory_key = request.memory_limit_key.clone().unwrap_or_else(|| {
        if config.memory_per_job_by_class.contains_key(&request.class) {
            format!(
                "policy.memory_per_job_by_class.{}*parallelism",
                request.class
            )
        } else {
            "policy.memory_limit_per_job_bytes*parallelism".to_owned()
        }
    });
    let killed = process(
        request,
        request.lease_record.is_none()
            || request
                .lease_record
                .as_ref()
                .is_some_and(|record| record.memory_enforcement == MemoryEnforcement::Refused),
    );
    let finish = |code, key: &str, limit: Value, value: Value, what: &str| {
        let killed = if code == Code::OomKilled {
            request
                .lease_record
                .as_ref()
                .and_then(|record| record.memory_scope.as_ref())
                .map(|scope| format!("scope:{scope}/{}", command_name(request)))
                .unwrap_or_else(|| killed.clone())
        } else {
            killed.clone()
        };
        let detail = if code == Code::SupervisionLost {
            request
                .supervision_detail
                .as_ref()
                .map(|detail| format!("{what}; detail={detail}"))
        } else {
            None
        };
        Diagnostic::failure(
            code,
            id,
            key,
            limit,
            value,
            detail.as_deref().unwrap_or(what),
            &killed,
        )
    };
    if let Some(record) = request
        .lease_record
        .as_ref()
        .filter(|record| record.oom_kill_count > 0 || record.scope_oom_killed)
    {
        return finish(
            Code::OomKilled,
            &memory_key,
            request.memory_cap_bytes.unwrap_or(0).into(),
            request
                .measured_memory_bytes
                .map_or_else(|| Value::from("unavailable"), Value::from),
            &format!(
                "rss_bytes; cgroup_oom_kill={}; cgroup_memory_peak_bytes={}",
                if record.oom_kill_count > 0 {
                    record.oom_kill_count.to_string()
                } else {
                    "unavailable; source=systemd.Result=oom-kill".to_owned()
                },
                record
                    .memory_sample
                    .as_ref()
                    .filter(|sample| matches!(
                        sample.metric,
                        crate::model::MemoryMetric::CgroupMemoryPeak
                    ))
                    .map_or_else(
                        || "unavailable".to_owned(),
                        |sample| sample.bytes.to_string()
                    )
            ),
        );
    }
    match request.stop {
        Some(StopReason::MemoryCap) => {
            let sample = request
                .lease_record
                .as_ref()
                .and_then(|record| record.memory_sample.as_ref());
            return finish(
                Code::RssCap,
                &memory_key,
                request.memory_cap_bytes.unwrap_or(0).into(),
                sample
                    .map(|sample| sample.bytes)
                    .or(request.measured_memory_bytes)
                    .map_or_else(|| Value::from("unavailable"), Value::from),
                sample.map_or("rss_bytes", |sample| sample.metric.as_str()),
            );
        }
        Some(StopReason::TimedOut) => {
            return finish(
                Code::HoldTimeout,
                "policy.hold_ms",
                config.hold_ms.into(),
                clock
                    .saturating_sub(request.started_clock_ms.unwrap_or(clock))
                    .into(),
                "elapsed_ms",
            );
        }
        Some(StopReason::Cancelled) => {
            return finish(
                Code::Cancelled,
                "policy.run_permitted",
                true.into(),
                false.into(),
                "cancelled by caller",
            );
        }
        None => {}
    }
    match request.reason.as_deref() {
        Some("rss_cap") => {
            let scoped = request
                .lease_record
                .as_ref()
                .is_some_and(|record| record.memory_enforcement == MemoryEnforcement::SystemdScope);
            return finish(
                if scoped {
                    Code::OomKilled
                } else {
                    Code::RssCap
                },
                &memory_key,
                request.memory_cap_bytes.unwrap_or(0).into(),
                if scoped {
                    Value::from(">=1")
                } else {
                    request
                        .measured_memory_bytes
                        .map_or_else(|| Value::from("unavailable"), Value::from)
                },
                if scoped {
                    "cgroup oom_kill counter"
                } else {
                    "rss_bytes"
                },
            );
        }
        Some("memory_cap_unavailable") => {
            return finish(
                Code::RefusedCapUnavailable,
                "policy.on_missing_cgroup",
                match config.on_missing_cgroup {
                    crate::config::OnMissingCgroup::Refuse => "refuse",
                    crate::config::OnMissingCgroup::RssWatchdog => "rss_watchdog",
                }
                .into(),
                "unavailable".into(),
                "required memory cap backend",
            );
        }
        Some("supervision_degraded") => {
            return finish(
                Code::SupervisionLost,
                "policy.supervision_required",
                true.into(),
                "degraded".into(),
                "supervision degraded; command exited 0",
            );
        }
        Some("supervisor_lost" | "supervision_lost" | "host_rebooted") => {
            let what = format!(
                "worker={} watchdog={}; command exit status not observed",
                request
                    .worker
                    .as_ref()
                    .map_or_else(|| "unavailable".to_owned(), |worker| worker.pid.to_string()),
                request.watchdog.as_ref().map_or_else(
                    || "unavailable".to_owned(),
                    |watchdog| watchdog.pid.to_string()
                )
            );
            return finish(
                Code::SupervisionLost,
                "policy.supervision_required",
                true.into(),
                false.into(),
                &what,
            );
        }
        _ => {}
    }
    if let Some(signal) = request.signal {
        return finish(
            Code::ExternalSignal,
            "policy.external_signals",
            "none".into(),
            Value::Integer(u64::try_from(signal).unwrap_or(0)),
            &format!("terminated by signal {signal} from outside locked-build"),
        );
    }
    if let Some(code) = request.exit_code.filter(|code| *code != 0) {
        return finish(
            Code::CommandExit,
            "policy.success_exit_code",
            0_u64.into(),
            Value::Integer(u64::try_from(code).unwrap_or(0)),
            "command exit status",
        );
    }
    if request.phase == Phase::Succeeded {
        return Diagnostic::default();
    }
    finish(
        Code::SupervisionLost,
        "policy.supervision_required",
        true.into(),
        "unavailable".into(),
        "command outcome unavailable",
    )
}

pub(crate) fn unscoped(error: &Error, request: String) -> Failure {
    if let Some(failure) = &error.failure {
        return failure.as_ref().clone();
    }
    let (code, key, limit) = if error.code == ErrorCode::Request {
        (
            Code::InvalidRequest,
            "policy.valid_request",
            Value::Boolean(true),
        )
    } else {
        (
            Code::StateUnavailable,
            "policy.state_access",
            Value::from("read_write"),
        )
    };
    Failure {
        request_id: request.clone(),
        class: "periodic".to_owned(),
        diagnostic: Diagnostic::failure(
            code,
            &request,
            key,
            limit,
            "unavailable".into(),
            &error.message,
            "not-started/command",
        ),
    }
}
