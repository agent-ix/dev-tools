// SPDX-License-Identifier: MIT
//! Hand-written operator guard and publication expectations.

use super::*;
use crate::model::{Phase, ProcessIdentity};

/// Provenance: SWM-421, SWM-400. Trace: FR-005-AC-4
#[cfg(target_os = "linux")]
#[test]
fn accepted_release_records_uid_reason_prior_stop_and_no_signal() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = root.path().join("policy.json");
    let mut policy: Config = serde_json::from_slice(include_bytes!("../../policy.example.json"))?;
    policy.hold_ms = 1;
    std::fs::write(&config, serde_json::to_vec(&policy)?)?;
    let store = crate::runtime::open(&root.path().join("state"), &config)?;
    crate::writable::install_host_namespace(&store.path)?;
    let id = crate::runtime::enqueue(
        &store,
        "test:team:lead",
        "SWM-421",
        1,
        0,
        None,
        &["/usr/bin/true".into()],
    )?;
    store.transaction(|state, _| {
        let request = state.find_mut(&id)?;
        request.enqueued_clock_ms = 0;
        request.stop = Some(crate::model::StopReason::TimedOut);
        Ok(())
    })?;
    let report = run(
        &store,
        Some(&id.0),
        Some("owner confirmed ended sandbox"),
        false,
    )?;
    assert_eq!(report.decisions[0].action, Action::Released);
    let (state, _) = store.snapshot()?;
    let request = state.find(&id)?;
    assert_eq!(request.phase, Phase::Failed);
    assert!(request.stop.is_none());
    let event = state
        .events
        .last()
        .ok_or_else(|| Error::new(ErrorCode::State, "audit event missing"))?;
    assert_eq!(event.kind, "operator_release");
    assert_eq!(
        event.trace.diagnostic.code,
        Some(locked_build::wire::Code::SupervisionLost)
    );
    assert_eq!(
        event.trace.diagnostic.killed.as_deref(),
        Some("not-signalled/operator-release")
    );
    let detail = event
        .trace
        .diagnostic
        .measured_what
        .as_deref()
        .unwrap_or_default();
    assert!(detail.contains(&format!("uid={}", nix::unistd::geteuid())));
    assert!(detail.contains("prior_stop=Some(TimedOut)"));
    assert!(detail.contains("reason=owner confirmed ended sandbox"));
    Ok(())
}

fn request() -> Request {
    let mut request = crate::metrics::tests::request(1, "host:team:owner".into(), Phase::Running);
    request.worker = Some(ProcessIdentity {
        pid: 8,
        started_seconds: 12,
    });
    request.pid_namespace = Some("pid:[foreign]".into());
    request.started_clock_ms = Some(100);
    request
}

/// Provenance: SWM-421, SWM-400. Trace: FR-005-AC-4
#[test]
fn operator_release_refuses_live_evidence_even_after_the_hold_deadline() {
    let request = request();
    let rows = [Observed {
        host_pid: 91,
        local_pid: Some(8),
        namespace: request.pid_namespace.clone(),
        started_seconds: Some(12),
        arguments: vec![],
        live: true,
    }];
    let decision = decide(&request, 1000, 2000, Some(true), &rows, true);
    assert_eq!(decision.action, Action::Refused);
    assert_eq!(decision.live_evidence, ["worker:host_pid=91"]);
    assert!(!decision.owner_worker_dead);
}

/// Provenance: SWM-421, SWM-400. Trace: FR-005-AC-4
#[test]
fn uncertain_owner_requires_expired_hold_but_a_dead_owner_can_release_earlier() {
    let request = request();
    let uncertain = [Observed {
        host_pid: 91,
        local_pid: None,
        namespace: None,
        started_seconds: None,
        arguments: vec![],
        live: true,
    }];
    assert_eq!(
        decide(&request, 1000, 200, None, &uncertain, true).action,
        Action::Refused
    );
    assert_eq!(
        decide(&request, 1000, 1100, None, &uncertain, true).action,
        Action::WouldRelease
    );
    let dead = decide(&request, 1000, 200, None, &[], true);
    assert_eq!(dead.action, Action::WouldRelease);
    assert!(dead.owner_worker_dead);
    assert!(!dead.hold_expired);
    assert_eq!(
        decide(&request, 1000, 200, None, &[], false).action,
        Action::Retain
    );
    assert_eq!(
        decide(&request, 1000, 200, Some(false), &[], false).action,
        Action::WouldDrain
    );
}

/// Provenance: SWM-421, SWM-400. Trace: FR-005-AC-4
#[test]
fn readable_request_identity_refuses_release_despite_birth_timestamp_drift() {
    let request = request();
    let rows = [Observed {
        host_pid: 91,
        local_pid: Some(8),
        namespace: None,
        started_seconds: Some(13),
        arguments: vec!["worker".into(), request.id.0.clone()],
        live: true,
    }];
    assert_eq!(
        decide(&request, 1000, 2000, None, &rows, true).action,
        Action::Refused
    );
}

/// Provenance: SWM-421, SWM-400. Trace: FR-005-AC-4
#[test]
fn unknown_or_changed_namespace_never_discards_live_request_identity() {
    let mut request = request();
    request.pid_namespace = None;
    let mut row = Observed {
        host_pid: 91,
        local_pid: Some(8),
        namespace: Some("pid:[visible]".into()),
        started_seconds: Some(12),
        arguments: vec![],
        live: true,
    };
    assert_eq!(
        decide(&request, 1000, 200, None, &[row], true).action,
        Action::Refused
    );
    row = Observed {
        host_pid: 91,
        local_pid: Some(8),
        namespace: Some("pid:[other]".into()),
        started_seconds: Some(99),
        arguments: vec!["worker".into(), request.id.0.clone()],
        live: true,
    };
    request.pid_namespace = Some("pid:[recorded]".into());
    assert_eq!(
        decide(&request, 1000, 2000, None, &[row], true).action,
        Action::Refused
    );
    request.pid_namespace = None;
    assert_eq!(
        decide(&request, 1000, 200, None, &[], true).action,
        Action::Refused
    );
}

/// Provenance: SWM-421, SWM-400. Trace: FR-005-AC-4
#[test]
fn tagged_processes_forbid_release_before_their_identity_is_registered() {
    for role in ["worker", "watchdog"] {
        let mut request = request();
        request.worker = None;
        request.watchdog = None;
        let rows = [Observed {
            host_pid: 91,
            local_pid: Some(8),
            namespace: None,
            started_seconds: Some(99),
            arguments: vec![role.into(), request.id.0.clone()],
            live: true,
        }];
        assert_eq!(
            decide(&request, 1000, 2000, None, &rows, true).action,
            Action::Refused,
            "{role}"
        );
    }
}

/// Provenance: SWM-421, SWM-400. Trace: FR-005-AC-4
#[cfg(target_os = "linux")]
#[test]
fn dry_run_and_rejected_release_leave_all_store_files_unchanged() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = root.path().join("policy.json");
    std::fs::write(&config, include_bytes!("../../policy.example.json"))?;
    let store = crate::runtime::open(&root.path().join("state"), &config)?;
    crate::writable::install_host_namespace(&store.path)?;
    let id = crate::runtime::enqueue(
        &store,
        "test:team:lead",
        "SWM-421",
        1,
        0,
        None,
        &["/usr/bin/true".into()],
    )?;
    let files = || -> Result<std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>> {
        std::fs::read_dir(&store.path)?
            .map(|entry| {
                let path = entry?.path();
                Ok((path.clone(), std::fs::read(path)?))
            })
            .collect()
    };
    let before = files()?;
    let dry = run(&store, Some(&id.0), Some("test operator decision"), true)?;
    assert_eq!(dry.decisions[0].action, Action::Refused);
    assert_eq!(before, files()?);
    let refused = run(&store, Some(&id.0), Some("test operator decision"), false)?;
    assert!(refused.refused());
    assert_eq!(before, files()?);
    assert!(run(&store, Some(&id.0), Some("\n"), true).is_err());
    assert_eq!(before, files()?);
    Ok(())
}
