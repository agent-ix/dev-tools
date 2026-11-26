// SPDX-License-Identifier: MIT
//! Process identity and conservative recovery of isolated supervisor groups.

use std::collections::BTreeMap;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, getpgid};
use sysinfo::{ProcessRefreshKind, ProcessStatus, RefreshKind, System};

use crate::error::{Error, ErrorCode, Result};
use crate::model::{ProcessIdentity, RequestId};

/// Only a kernel ESRCH result proves that no process can still fork in a group.
/// Permission errors and reused group IDs retain occupancy conservatively.
pub(crate) fn group_absent(group: u32) -> bool {
    i32::try_from(group).ok().is_some_and(|group| {
        group > 0 && killpg(Pid::from_raw(group), None) == Err(nix::errno::Errno::ESRCH)
    })
}

pub(crate) struct Processes {
    rows: BTreeMap<u32, Row>,
    complete: bool,
}

struct Row {
    start: u64,
    group: Option<u32>,
    memory_bytes: u64,
    arguments: Vec<String>,
    zombie: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdentityState {
    Matching,
    Absent,
    Different,
    Unavailable,
}

impl Processes {
    pub(crate) fn read() -> Self {
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
        );
        let rows: BTreeMap<u32, Row> = system
            .processes()
            .iter()
            .map(|(pid, process)| {
                let number = pid.as_u32();
                let native_group = i32::try_from(number)
                    .ok()
                    .map(|number| getpgid(Some(Pid::from_raw(number))));
                let gone = matches!(native_group, Some(Err(nix::errno::Errno::ESRCH)));
                let group = native_group
                    .and_then(std::result::Result::ok)
                    .and_then(|group| u32::try_from(group.as_raw()).ok());
                (
                    number,
                    Row {
                        start: process.start_time(),
                        group,
                        memory_bytes: process.memory(),
                        arguments: process
                            .cmd()
                            .iter()
                            .map(|arg| arg.to_string_lossy().into_owned())
                            .collect(),
                        zombie: process.status() == ProcessStatus::Zombie || gone,
                    },
                )
            })
            .collect();
        let complete = rows.contains_key(&std::process::id());
        Self { rows, complete }
    }

    pub(crate) fn identity(&self, identity: &ProcessIdentity, id: &RequestId) -> IdentityState {
        self.role_identity(identity, id, "worker", identity.pid)
    }

    pub(crate) fn role_identity(
        &self,
        identity: &ProcessIdentity,
        id: &RequestId,
        role: &str,
        group: u32,
    ) -> IdentityState {
        if !self.complete {
            return IdentityState::Unavailable;
        }
        let Some(row) = self.rows.get(&identity.pid) else {
            return IdentityState::Absent;
        };
        if row.zombie {
            return IdentityState::Absent;
        }
        if row.arguments.is_empty() || row.group.is_none() {
            return IdentityState::Unavailable;
        }
        if row.start == identity.started_seconds
            && row.group == Some(group)
            && row.arguments.iter().any(|argument| argument == &id.0)
            && row.arguments.iter().any(|argument| argument == role)
        {
            IdentityState::Matching
        } else {
            IdentityState::Different
        }
    }

    pub(crate) fn current(&self) -> Result<ProcessIdentity> {
        self.current_member(std::process::id())
    }

    pub(crate) fn current_member(&self, group: u32) -> Result<ProcessIdentity> {
        let pid = std::process::id();
        let row = self
            .rows
            .get(&pid)
            .ok_or_else(|| Error::new(ErrorCode::State, "cannot verify supervisor identity"))?;
        if row.group != Some(group) {
            return Err(Error::new(
                ErrorCode::State,
                "supervisor must own an isolated process group",
            ));
        }
        Ok(ProcessIdentity {
            pid,
            started_seconds: row.start,
        })
    }

    pub(crate) fn can_sample_memory(&self) -> bool {
        self.complete
    }

    pub(crate) fn group_has_live_members(&self, group: u32, excluding: Option<u32>) -> bool {
        self.group_has_live_members_excluding(group, excluding.as_slice())
    }

    pub(crate) fn group_has_live_members_excluding(&self, group: u32, excluding: &[u32]) -> bool {
        if !self.complete {
            return true;
        }
        self.rows.iter().any(|(pid, row)| {
            (row.group == Some(group) || row.group.is_none())
                && !row.zombie
                && !excluding.contains(pid)
        })
    }

    pub(crate) fn group_memory_bytes(&self, group: u32, excluding: &[u32]) -> Option<u64> {
        self.complete.then(|| {
            self.rows
                .iter()
                .filter(|(pid, row)| {
                    row.group == Some(group) && !row.zombie && !excluding.contains(pid)
                })
                .map(|(_, row)| row)
                .fold(0_u64, |sum, row| sum.saturating_add(row.memory_bytes))
        })
    }

    pub(crate) fn signal_member_owned(
        &self,
        identity: &ProcessIdentity,
        id: &RequestId,
        role: &str,
        group: u32,
        signal: Signal,
    ) -> Result<()> {
        if identity.pid != std::process::id()
            || self.role_identity(identity, id, role, group) != IdentityState::Matching
        {
            return Err(Error::new(
                ErrorCode::State,
                "process identity unverified; refusing group signal",
            ));
        }
        let pid = i32::try_from(group)
            .map_err(|_| Error::new(ErrorCode::State, "process ID out of range"))?;
        killpg(Pid::from_raw(pid), signal)
            .map_err(|error| Error::new(ErrorCode::Io, format!("group signal failed: {error}")))
    }

    pub(crate) fn group_oom_kill_count(&self, group: u32) -> Option<u64> {
        if !self.complete {
            return None;
        }
        self.rows
            .iter()
            .filter(|(_, row)| row.group == Some(group) && !row.zombie)
            .filter_map(|(pid, _)| cgroup_oom_kill_count(*pid))
            .max()
    }
}

fn cgroup_oom_kill_count(pid: u32) -> Option<u64> {
    let membership = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let path = std::path::Path::new("/sys/fs/cgroup")
        .join(relative.trim_start_matches('/'))
        .join("memory.events");
    let events = std::fs::read_to_string(path).ok()?;
    parse_oom_kill_count(&events)
}

fn parse_oom_kill_count(events: &str) -> Option<u64> {
    events.lines().find_map(|line| {
        let (name, value) = line.split_once(char::is_whitespace)?;
        (name == "oom_kill")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trace: FR-005-AC-4
    #[test]
    fn unavailable_process_population_never_proves_group_exit() {
        let processes = Processes {
            rows: BTreeMap::new(),
            complete: false,
        };
        assert!(processes.group_has_live_members(77, None));
        assert_eq!(
            processes.identity(
                &ProcessIdentity {
                    pid: 77,
                    started_seconds: 1
                },
                &RequestId("token".into())
            ),
            IdentityState::Unavailable
        );
    }

    /// Trace: FR-005-AC-4
    #[test]
    fn reused_pid_never_authorizes_a_signal_or_claims_exit() {
        let processes = Processes {
            complete: true,
            rows: BTreeMap::from([(
                77,
                Row {
                    start: 100,
                    group: Some(77),
                    memory_bytes: 4_096,
                    arguments: vec!["unrelated".into()],
                    zombie: false,
                },
            )]),
        };
        let identity = ProcessIdentity {
            pid: 77,
            started_seconds: 100,
        };
        let id = RequestId("secret-token".into());
        assert_eq!(processes.identity(&identity, &id), IdentityState::Different);
        assert!(processes.group_has_live_members(77, None));
        assert_eq!(
            processes
                .signal_member_owned(&identity, &id, "worker", identity.pid, Signal::SIGTERM)
                .map_err(|error| error.code),
            Err(ErrorCode::State)
        );
    }

    #[test]
    fn rss_fallback_sums_only_live_group_members_and_excludes_watchdog() {
        let processes = Processes {
            complete: true,
            rows: BTreeMap::from([
                (
                    77,
                    Row {
                        start: 1,
                        group: Some(77),
                        memory_bytes: 100,
                        arguments: vec!["watchdog".into()],
                        zombie: false,
                    },
                ),
                (
                    78,
                    Row {
                        start: 1,
                        group: Some(77),
                        memory_bytes: 600,
                        arguments: vec!["cargo".into()],
                        zombie: false,
                    },
                ),
                (
                    79,
                    Row {
                        start: 1,
                        group: Some(79),
                        memory_bytes: 900,
                        arguments: vec!["unrelated".into()],
                        zombie: false,
                    },
                ),
            ]),
        };
        assert_eq!(processes.group_memory_bytes(77, &[77]), Some(600));
    }

    #[test]
    fn cgroup_oom_kill_event_is_distinguished_from_a_manual_signal() {
        assert_eq!(
            parse_oom_kill_count("low 0\nhigh 0\nmax 1\noom 1\noom_kill 1\n"),
            Some(1)
        );
        assert_eq!(
            parse_oom_kill_count("low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\n"),
            Some(0)
        );
        assert_eq!(parse_oom_kill_count("low 0\nmax 1\n"), None);
    }
}
