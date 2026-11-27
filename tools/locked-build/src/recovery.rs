// SPDX-License-Identifier: MIT
//! Read-only recovery previews and explicitly audited operator releases.

use std::path::Path;

use serde::Serialize;
use sysinfo::{ProcessRefreshKind, ProcessStatus, RefreshKind, System};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Completion, Request, RequestId, State};
use crate::process::NamespaceSnapshot;
use crate::store::Store;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Retain,
    WouldDrain,
    WouldRelease,
    Released,
    Refused,
}

#[derive(Debug, Serialize)]
pub(crate) struct Decision {
    pub request_id: String,
    pub action: Action,
    pub namespace_alive: Option<bool>,
    pub owner_worker_dead: bool,
    pub hold_expired: bool,
    pub live_evidence: Vec<String>,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Report {
    pub dry_run: bool,
    pub decisions: Vec<Decision>,
}

impl Report {
    pub(crate) fn refused(&self) -> bool {
        self.decisions
            .iter()
            .any(|row| row.action == Action::Refused)
    }
}

struct Observed {
    host_pid: u32,
    local_pid: Option<u32>,
    namespace: Option<String>,
    started_seconds: Option<u64>,
    arguments: Vec<String>,
    live: bool,
}

fn observations() -> Result<Vec<Observed>> {
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::everything().without_tasks()),
    );
    let mut rows = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Ok(host_pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let process = system.process(sysinfo::Pid::from_u32(host_pid));
        let status = std::fs::read_to_string(entry.path().join("status")).ok();
        let local_pid = status.as_deref().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("NSpid:"))?
                .split_whitespace()
                .last()?
                .parse()
                .ok()
        });
        let namespace = std::fs::read_link(entry.path().join("ns/pid"))
            .ok()
            .map(|value| value.to_string_lossy().into_owned());
        // A vanished directory plus the independent kernel probe proves the
        // process cannot register or fork after this observation.
        if !entry.path().exists()
            && i32::try_from(host_pid).is_ok_and(|pid| {
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None)
                    == Err(nix::errno::Errno::ESRCH)
            })
        {
            continue;
        }
        rows.push(Observed {
            host_pid,
            local_pid,
            namespace,
            started_seconds: process.map(|row| row.start_time()),
            arguments: process
                .map(|row| {
                    row.cmd()
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default(),
            live: process.is_none_or(|row| row.status() != ProcessStatus::Zombie),
        });
    }
    Ok(rows)
}

fn decide(
    request: &Request,
    hold_ms: u64,
    clock: u64,
    namespace_alive: Option<bool>,
    rows: &[Observed],
    operator: bool,
) -> Decision {
    let mut live_evidence = Vec::new();
    let mut owner_unknown = request.worker.is_none() || request.pid_namespace.is_none();
    let mut worker_live = false;
    for (role, identity) in [
        ("worker", request.worker.as_ref()),
        ("watchdog", request.watchdog.as_ref()),
        (
            "command",
            request
                .lease_record
                .as_ref()
                .and_then(|record| record.command.as_ref()),
        ),
    ] {
        for row in rows.iter().filter(|row| row.live) {
            let tagged = row.arguments.iter().any(|arg| arg == &request.id.0)
                && row.arguments.iter().any(|arg| arg == role);
            if !tagged
                && row
                    .namespace
                    .as_ref()
                    .zip(request.pid_namespace.as_ref())
                    .is_some_and(|(actual, expected)| actual != expected)
            {
                continue;
            }
            let birth_matches = identity
                .is_some_and(|identity| row.started_seconds == Some(identity.started_seconds));
            if identity.is_some_and(|identity| row.local_pid == Some(identity.pid) && birth_matches)
                || tagged
            {
                live_evidence.push(format!("{role}:host_pid={}", row.host_pid));
                worker_live |= role == "worker";
            } else if role == "worker"
                && (row.started_seconds.is_none() || birth_matches || tagged)
                && identity.is_some_and(|identity| {
                    row.local_pid.is_none() || row.local_pid == Some(identity.pid)
                })
            {
                owner_unknown = true;
            }
        }
    }
    let owner_worker_dead = namespace_alive == Some(false) || (!worker_live && !owner_unknown);
    let hold_expired = clock.saturating_sub(
        request
            .started_clock_ms
            .unwrap_or(request.enqueued_clock_ms),
    ) >= hold_ms;
    let (action, detail) = if request.phase.terminal() {
        (Action::Refused, "request is already terminal")
    } else if operator {
        if !live_evidence.is_empty() {
            (
                Action::Refused,
                "live lease process evidence forbids release",
            )
        } else if !hold_expired && !owner_worker_dead {
            (
                Action::Refused,
                "release requires an expired hold or a provably dead owner worker",
            )
        } else {
            (
                Action::WouldRelease,
                "explicit operator release; uncertainty is retained in this preview",
            )
        }
    } else if namespace_alive == Some(false) && live_evidence.is_empty() {
        (Action::WouldDrain, "namespace init is provably absent")
    } else {
        (
            Action::Retain,
            "namespace is live or its extinction remains unverified",
        )
    };
    Decision {
        request_id: request.id.0.clone(),
        action,
        namespace_alive,
        owner_worker_dead,
        hold_expired,
        live_evidence,
        detail: detail.into(),
    }
}

fn preview(
    state: &State,
    config: &Config,
    store_path: &Path,
    release: Option<&str>,
) -> Result<Report> {
    let namespace = crate::process::pid_namespace()?;
    let host = crate::writable::is_installed_host_namespace(
        store_path,
        crate::process::boot_id().as_deref(),
        namespace.as_deref(),
    );
    if !host {
        return Err(Error::new(
            ErrorCode::Request,
            "recovery preview/release requires the installed host PID namespace",
        ));
    }
    let namespaces: NamespaceSnapshot = crate::process::namespaces_at(Path::new("/proc"))?;
    let rows = observations()?;
    let clock = crate::runtime::clock_ms()?;
    let selected: Vec<_> = match release {
        Some(id) => vec![state.find(&RequestId(id.into()))?],
        None => state
            .requests
            .iter()
            .filter(|request| !request.phase.terminal() && request.pid_namespace != namespace)
            .collect(),
    };
    Ok(Report {
        dry_run: true,
        decisions: selected
            .into_iter()
            .map(|request| {
                let alive = request
                    .pid_namespace
                    .as_deref()
                    .and_then(|ns| namespaces.alive(ns));
                decide(
                    request,
                    config.hold_ms,
                    clock,
                    alive,
                    &rows,
                    release.is_some(),
                )
            })
            .collect(),
    })
}

pub(crate) fn run(
    store: &Store,
    release: Option<&str>,
    reason: Option<&str>,
    dry_run: bool,
) -> Result<Report> {
    if release.is_some() != reason.is_some()
        || reason.is_some_and(|text| {
            text.trim().is_empty() || text.len() > 1024 || text.chars().any(char::is_control)
        })
    {
        return Err(Error::new(
            ErrorCode::Request,
            "--release requires a nonempty --reason of at most 1024 bytes without control characters",
        ));
    }
    if dry_run {
        let (state, config) = store.snapshot()?;
        return preview(&state, &config, &store.path, release);
    }
    let id = release
        .ok_or_else(|| Error::new(ErrorCode::Request, "operator release requires --release"))?;
    let (state, config) = store.snapshot()?;
    let mut preflight = preview(&state, &config, &store.path, Some(id))?;
    preflight.dry_run = false;
    if preflight.refused() {
        return Ok(preflight);
    }
    store.transaction(|state, config| {
        let mut report = preview(state, config, &store.path, Some(id))?;
        report.dry_run = false;
        if report.refused() {
            return Ok(report);
        }
        let id = RequestId(id.into());
        let request = state.find_mut(&id)?;
        request.supervision_detail = Some(format!(
            "operator release uid={}; prior_stop={:?}; reason={}",
            nix::unistd::geteuid(),
            request.stop,
            reason.unwrap_or_default()
        ));
        request.stop = None;
        state.finish(
            &id,
            Completion {
                reason: Some("operator_release"),
                ..Completion::default()
            },
            config,
            crate::runtime::now_ms()?,
            crate::runtime::clock_ms()?,
        )?;
        if let Some(event) = state.events.last_mut() {
            event.kind = "operator_release".into();
        }
        report.dry_run = false;
        for row in &mut report.decisions {
            row.action = Action::Released;
        }
        Ok(report)
    })
}

#[cfg(test)]
mod tests;
