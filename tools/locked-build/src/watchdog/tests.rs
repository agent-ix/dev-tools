// SPDX-License-Identifier: MIT
//! Controlled command birth and descendant fork after a process snapshot.

use super::*;
use crate::model::ProcessIdentity;

/// Provenance: SWM-400. Trace: FR-005-AC-2, FR-005-AC-4
#[test]
fn orphan_without_client_or_command_releases_lease_in_one_poll() -> Result<()> {
    for (control, done) in [
        ("orphan", true),
        ("live-client", false),
        ("live-command", false),
        ("launched-orphan", true),
        ("successful-command", true),
        ("foreign-client", false),
        ("kernel-occupied", false),
    ] {
        let root = tempfile::tempdir()?;
        let policy = root.path().join("policy.json");
        std::fs::write(&policy, include_bytes!("../../policy.example.json"))?;
        let store = Store::open(&root.path().join("state"), &policy)?;
        let id =
            crate::runtime::enqueue(&store, "owner", "SWM-400", 1, 0, None, &["cargo".into()])?;
        let worker = ProcessIdentity {
            pid: 10_000,
            started_seconds: 1,
        };
        let watchdog = ProcessIdentity {
            pid: 20_000,
            started_seconds: 1,
        };
        store.transaction(|state, _| {
            let row = state.find_mut(&id)?;
            row.client = Some(ProcessIdentity {
                pid: 2_000_000_000,
                started_seconds: 1,
            });
            if control == "foreign-client" {
                row.client_pid_namespace = Some("foreign-namespace".into());
            }
            if matches!(control, "launched-orphan" | "successful-command") {
                row.command_started_clock_ms = Some(60);
                row.lease_record = Some(crate::model::LeaseRecord {
                    command: Some(ProcessIdentity {
                        pid: 2_000_000_001,
                        started_seconds: 1,
                    }),
                    ..crate::model::LeaseRecord::default()
                });
                if control == "successful-command" {
                    row.command_finished = Some(crate::model::CommandOutcome {
                        code: Some(0),
                        signal: None,
                    });
                }
            }
            row.worker = Some(worker.clone());
            row.watchdog = Some(watchdog.clone());
            row.worker_generation = 1;
            row.phase = Phase::Running;
            row.started_at_ms = Some(50);
            row.started_clock_ms = Some(50);
            Ok(())
        })?;
        let decision = observe_at(
            &store,
            &id,
            1,
            &worker,
            &watchdog,
            100,
            100,
            || {
                let mut rows = vec![
                    (
                        worker.clone(),
                        worker.pid,
                        vec![id.0.clone(), "worker".into()],
                    ),
                    (
                        watchdog.clone(),
                        watchdog.pid,
                        vec![id.0.clone(), "watchdog".into()],
                    ),
                ];
                if control == "live-client" {
                    rows.push((
                        ProcessIdentity {
                            pid: 2_000_000_000,
                            started_seconds: 1,
                        },
                        2_000_000_000,
                        vec!["caller".into()],
                    ));
                }
                if control == "live-command" {
                    rows.push((
                        ProcessIdentity {
                            pid: 20_001,
                            started_seconds: 1,
                        },
                        20_000,
                        vec!["rustc".into()],
                    ));
                }
                Processes::test_snapshot(rows)
            },
            |_| Ok(control != "kernel-occupied"),
        )?;
        assert_eq!(matches!(decision, Decision::Done), done, "{control}");
        let (state, _) = store.snapshot()?;
        assert_eq!(state.used(), u32::from(!done), "{control}");
        assert_eq!(
            state.find(&id)?.trace.diagnostic.code,
            (done && control != "successful-command")
                .then_some(locked_build::wire::Code::SupervisionLost),
            "{control}"
        );
    }
    Ok(())
}

/// Provenance: SWM-380. Trace: FR-005-AC-2
#[test]
fn watchdog_startup_pins_the_admitted_store_and_policy_paths() -> Result<()> {
    let root = tempfile::tempdir()?;
    let policy = root.path().join("policy.json");
    std::fs::write(&policy, include_bytes!("../../policy.example.json"))?;
    let store = Store::open(&root.path().join("state"), &policy)?;
    let command = startup_command(&store, &RequestId("store-a:r7".into()), 3)?;
    let actual: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        actual,
        [
            "--state".to_owned(),
            store.path.display().to_string(),
            "--config".to_owned(),
            policy.canonicalize()?.display().to_string(),
            "watchdog".to_owned(),
            "store-a:r7".to_owned(),
            "3".to_owned()
        ]
    );
    Ok(())
}

#[test]
fn only_rss_watchdog_enforcement_uses_the_process_rss_cap() {
    assert_eq!(
        rss_fallback_cap(MemoryEnforcement::RssWatchdog, Some(100)),
        Some(100)
    );
    assert_eq!(rss_fallback_cap(MemoryEnforcement::RssWatchdog, None), None);
    assert_eq!(
        rss_fallback_cap(MemoryEnforcement::SystemdScope, Some(100)),
        None
    );
}

fn simulated_processes(
    id: &RequestId,
    watchdog: &ProcessIdentity,
    command_members: u32,
) -> Processes {
    let watchdog_arguments = vec![id.0.clone(), "watchdog".to_owned()];
    let mut rows = vec![(watchdog.clone(), watchdog.pid, watchdog_arguments)];
    rows.extend((0..command_members).map(|index| {
        let command = ProcessIdentity {
            pid: watchdog.pid + index + 1,
            started_seconds: 1,
        };
        (command, watchdog.pid, vec!["cargo-command".to_owned()])
    }));
    Processes::test_snapshot(rows)
}

fn scenario(command_members: u32) -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = root.path().join("policy.json");
    std::fs::write(&config, include_bytes!("../../policy.example.json"))?;
    let store = Store::open(&root.path().join("state"), &config)?;
    let id = crate::runtime::enqueue(
        &store,
        "test:team:lead",
        "TEST-1",
        1,
        0,
        None,
        &["unused".into()],
    )?;
    let worker = ProcessIdentity {
        pid: 10_000,
        started_seconds: 1,
    };
    let watchdog = ProcessIdentity {
        pid: 20_000,
        started_seconds: 1,
    };
    let now = crate::runtime::now_ms()?;
    let clock = crate::runtime::clock_ms()?;
    store.transaction_with_clock(
        || Ok((now, clock)),
        |state, _, _, _| {
            let request = state.find_mut(&id)?;
            request.worker = Some(worker.clone());
            request.worker_generation = 1;
            request.watchdog = Some(watchdog.clone());
            request.phase = Phase::Running;
            request.started_at_ms = Some(now);
            request.started_clock_ms = Some(clock);
            Ok(())
        },
    )?;

    // The first process snapshot predates the command birth. The kernel probe
    // still reports the group occupied, so the lease remains charged.
    let decision = observe_at(
        &store,
        &id,
        1,
        &worker,
        &watchdog,
        now,
        clock,
        || simulated_processes(&id, &watchdog, 0),
        |_| Ok(false),
    )?;
    let occupied =
        store.transaction_with_clock(|| Ok((now, clock)), |state, _, _, _| Ok(state.used()))?;
    assert_eq!(
        occupied, 1,
        "a missed live command released occupied capacity"
    );
    assert!(
        matches!(decision, Decision::Observe { .. }),
        "a missed live command completed the lease"
    );

    let with_command = observe_at(
        &store,
        &id,
        1,
        &worker,
        &watchdog,
        now,
        clock,
        || simulated_processes(&id, &watchdog, command_members),
        |_| Ok(true),
    )?;
    assert!(matches!(with_command, Decision::Observe { .. }));

    let after_exit = observe_at(
        &store,
        &id,
        1,
        &worker,
        &watchdog,
        now,
        clock,
        || simulated_processes(&id, &watchdog, 0),
        |group| Ok(group == watchdog.pid),
    )?;
    assert!(matches!(after_exit, Decision::Done));
    store.transaction_with_clock(
        || Ok((now, clock)),
        |state, _, _, _| {
            assert_eq!(state.used(), 0);
            Ok(())
        },
    )
}

/// Trace: FR-005-AC-2, FR-005-AC-4
#[test]
fn missed_command_birth_cannot_release_occupied_watchdog_lease() -> Result<()> {
    scenario(1)
}

/// Trace: FR-005-AC-2, FR-005-AC-4
#[test]
fn missed_descendant_fork_and_parent_exit_cannot_release_occupied_watchdog_lease() -> Result<()> {
    scenario(2)
}
