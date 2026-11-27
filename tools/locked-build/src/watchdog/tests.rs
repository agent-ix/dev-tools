// SPDX-License-Identifier: MIT
//! Controlled command birth and descendant fork after a process snapshot.

use super::*;
use crate::model::ProcessIdentity;

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
