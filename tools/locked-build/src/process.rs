// SPDX-License-Identifier: MIT
//! Process identity and conservative recovery of isolated supervisor groups.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, getpgid};
use sysinfo::{ProcessRefreshKind, ProcessStatus, RefreshKind, System};

use crate::error::{Error, ErrorCode, Result};
use crate::model::{ProcessIdentity, RequestId};

pub(crate) fn boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub(crate) fn pid_namespace() -> Result<Option<String>> {
    if cfg!(target_os = "linux") {
        Ok(Some(
            std::fs::read_link("/proc/self/ns/pid")?
                .to_string_lossy()
                .into_owned(),
        ))
    } else {
        Ok(None)
    }
}

pub(crate) fn watchdog_memory_metric() -> crate::model::MemoryMetric {
    if cfg!(target_os = "linux") {
        crate::model::MemoryMetric::PssAnon
    } else {
        crate::model::MemoryMetric::Rss
    }
}

pub(crate) fn owned_child_exited(child: &std::process::Child) -> Result<bool> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    // NOWAIT pins the PID until cleanup/reaping; procfs argv visibility is not
    // required to establish ownership of a direct Child handle.
    waitid(
        WaitId::Pid(Pid::from_child(child)),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .map(|status| status.is_some())
    .map_err(|error| Error::new(ErrorCode::Io, error.to_string()))
}

pub(crate) fn child_is_owned(child: &std::process::Child) -> Result<bool> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    match waitid(
        WaitId::Pid(Pid::from_child(child)),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    ) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::CHILD) => Ok(false),
        Err(error) => Err(Error::new(ErrorCode::Io, error.to_string())),
    }
}

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
    parent: Option<u32>,
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
    #[cfg(test)]
    pub(crate) fn test_snapshot(
        rows: impl IntoIterator<Item = (ProcessIdentity, u32, Vec<String>)>,
    ) -> Self {
        Self {
            rows: rows
                .into_iter()
                .map(|(identity, group, arguments)| {
                    (
                        identity.pid,
                        Row {
                            start: identity.started_seconds,
                            group: Some(group),
                            parent: None,
                            memory_bytes: 0,
                            arguments,
                            zombie: false,
                        },
                    )
                })
                .collect(),
            complete: true,
        }
    }

    pub(crate) fn read() -> Self {
        let system = System::new_with_specifics(
            // Linux tasks share one address space. Including TIDs counts a
            // rustc's memory once per compiler thread instead of once per PID.
            RefreshKind::nothing().with_processes(ProcessRefreshKind::everything().without_tasks()),
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
                        parent: process.parent().map(|parent| parent.as_u32()),
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
            && (!cfg!(target_os = "linux")
                || anonymous_memory_bytes(std::path::Path::new("/proc"), std::process::id())
                    .is_some())
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

    pub(crate) fn member_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        let row = self.rows.get(&pid).filter(|row| !row.zombie)?;
        Some(ProcessIdentity {
            pid,
            started_seconds: row.start,
        })
    }

    pub(crate) fn command_memory_sample(
        &self,
        root: &ProcessIdentity,
    ) -> Option<crate::model::MemorySample> {
        self.command_memory_sample_with(root, watchdog_memory_metric(), |pid, row| {
            if cfg!(target_os = "linux") {
                anonymous_memory_bytes(std::path::Path::new("/proc"), pid)
            } else {
                Some(row.memory_bytes)
            }
        })
    }

    fn command_memory_sample_with(
        &self,
        root: &ProcessIdentity,
        metric: crate::model::MemoryMetric,
        mut memory: impl FnMut(u32, &Row) -> Option<u64>,
    ) -> Option<crate::model::MemorySample> {
        if !self.complete || self.rows.get(&root.pid)?.start != root.started_seconds {
            return None;
        }
        let mut members = BTreeSet::from([root.pid]);
        let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for (pid, row) in &self.rows {
            if let Some(parent) = row.parent {
                children.entry(parent).or_default().push(*pid);
            }
        }
        let mut pending: Vec<_> = members.iter().copied().collect();
        while let Some(parent) = pending.pop() {
            for child in children.get(&parent).into_iter().flatten() {
                if members.insert(*child) {
                    pending.push(*child);
                }
            }
        }
        let mut total = 0_u64;
        let mut contributors = Vec::new();
        for (pid, row) in &self.rows {
            if members.contains(pid) && !row.zombie {
                let bytes = memory(*pid, row)?;
                total = total.checked_add(bytes)?;
                contributors.push(crate::model::ProcessMemory { pid: *pid, bytes });
            }
        }
        Some(crate::model::MemorySample {
            bytes: total,
            metric,
            contributors,
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
        let own_group = group == identity.pid
            && u32::try_from(nix::unistd::getpgrp().as_raw()).ok() == Some(group);
        if identity.pid != std::process::id()
            || (!own_group
                && self.role_identity(identity, id, role, group) != IdentityState::Matching)
        {
            return Err(Error::new(
                ErrorCode::State,
                "process identity unverified; refusing group signal",
            ));
        }
        // The current process pins its own PID/PGID even if procfs stops
        // exposing argv. Losing that visibility must not disable cleanup.
        let pid = i32::try_from(group)
            .map_err(|_| Error::new(ErrorCode::State, "process ID out of range"))?;
        killpg(Pid::from_raw(pid), signal)
            .map_err(|error| Error::new(ErrorCode::Io, format!("group signal failed: {error}")))
    }

    pub(crate) fn group_memory_events_files(&self, group: u32) -> BTreeSet<PathBuf> {
        self.group_memory_events_files_with(group, cgroup_memory_events_file)
    }

    fn group_memory_events_files_with(
        &self,
        group: u32,
        mut events_file: impl FnMut(u32) -> Option<PathBuf>,
    ) -> BTreeSet<PathBuf> {
        if !self.complete {
            return BTreeSet::new();
        }
        // The watchdog remains outside the lease's systemd scope. Its inherited
        // cgroup can contain old or unrelated OOM counters from the outer lane.
        let inherited = events_file(group);
        self.rows
            .iter()
            .filter(|(_, row)| row.group == Some(group) && !row.zombie)
            .filter_map(|(pid, _)| events_file(*pid))
            .filter(|path| Some(path) != inherited.as_ref())
            .collect()
    }
}

fn anonymous_memory_bytes(proc_root: &std::path::Path, pid: u32) -> Option<u64> {
    let path = proc_root.join(pid.to_string()).join("smaps_rollup");
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let native = i32::try_from(pid).ok()?;
            return (getpgid(Some(Pid::from_raw(native))) == Err(nix::errno::Errno::ESRCH))
                .then_some(0);
        }
        Err(_) => return None,
    };
    let mut fields = contents
        .lines()
        .filter_map(|line| line.strip_prefix("Pss_Anon:"));
    let mut value = fields.next()?.split_whitespace();
    let kib = value.next()?.parse::<u64>().ok()?;
    if value.next()? != "kB" || value.next().is_some() || fields.next().is_some() {
        return None;
    }
    kib.checked_mul(1024)
}

fn cgroup_memory_events_file(pid: u32) -> Option<PathBuf> {
    let membership = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    Some(
        std::path::Path::new("/sys/fs/cgroup")
            .join(relative.trim_start_matches('/'))
            .join("memory.events"),
    )
}

pub(crate) fn oom_kill_count_from_file(path: &std::path::Path) -> Option<u64> {
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

    /// Provenance: SWM-380. Trace: FR-005-AC-4
    #[cfg(target_os = "linux")]
    #[test]
    fn process_snapshot_counts_a_threaded_address_space_once() -> Result<()> {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let child_barrier = barrier.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let tid = nix::unistd::gettid().as_raw();
            let _ = send.send(tid);
            child_barrier.wait();
        });
        let tid = receive
            .recv()
            .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
        let processes = Processes::read();
        barrier.wait();
        worker
            .join()
            .map_err(|_| Error::new(ErrorCode::State, "thread fixture panicked"))?;
        assert!(processes.rows.contains_key(&std::process::id()));
        assert!(!processes.rows.contains_key(
            &u32::try_from(tid).map_err(|error| Error::new(ErrorCode::State, error.to_string()))?
        ));
        Ok(())
    }

    /// Provenance: SWM-379. Trace: FR-005-AC-4
    #[test]
    fn scope_events_exclude_the_watchdogs_inherited_cgroup() {
        let processes = Processes::test_snapshot([
            (
                ProcessIdentity {
                    pid: 77,
                    started_seconds: 1,
                },
                77,
                vec![],
            ),
            (
                ProcessIdentity {
                    pid: 78,
                    started_seconds: 1,
                },
                77,
                vec![],
            ),
            (
                ProcessIdentity {
                    pid: 79,
                    started_seconds: 1,
                },
                79,
                vec![],
            ),
        ]);
        let files = processes.group_memory_events_files_with(77, |pid| match pid {
            77 => Some("/sys/fs/cgroup/session.scope/memory.events".into()),
            78 => Some("/sys/fs/cgroup/run-u123.scope/memory.events".into()),
            79 => Some("/sys/fs/cgroup/run-u456.scope/memory.events".into()),
            _ => None,
        });
        assert_eq!(
            files,
            BTreeSet::from([PathBuf::from("/sys/fs/cgroup/run-u123.scope/memory.events")])
        );
    }

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
                    parent: None,
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
    fn rss_fallback_counts_only_the_command_tree_not_its_shared_group() {
        let processes = Processes {
            complete: true,
            rows: BTreeMap::from([
                (
                    77,
                    Row {
                        start: 1,
                        group: Some(77),
                        memory_bytes: 100,
                        parent: None,
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
                        parent: None,
                        arguments: vec!["cargo".into()],
                        zombie: false,
                    },
                ),
                (
                    79,
                    Row {
                        start: 1,
                        group: Some(77),
                        memory_bytes: 900,
                        parent: None,
                        arguments: vec!["unrelated".into()],
                        zombie: false,
                    },
                ),
            ]),
        };
        assert_eq!(
            processes
                .command_memory_sample_with(
                    &ProcessIdentity {
                        pid: 78,
                        started_seconds: 1
                    },
                    crate::model::MemoryMetric::Rss,
                    |_, row| Some(row.memory_bytes)
                )
                .map(|sample| sample.bytes),
            Some(600)
        );
    }

    /// Provenance: SWM-380. Trace: FR-005-AC-4
    #[test]
    fn linux_anonymous_accounting_excludes_shared_file_and_tmpfs_mappings() -> Result<()> {
        let root = tempfile::tempdir()?;
        for pid in [78, 79] {
            let directory = root.path().join(pid.to_string());
            std::fs::create_dir(&directory)?;
            std::fs::write(
                directory.join("smaps_rollup"),
                "Rss: 1000 kB\nPss: 600 kB\nPss_Anon: 100 kB\nPss_File: 200 kB\nPss_Shmem: 300 kB\n",
            )?;
        }
        let mut processes = Processes::test_snapshot([
            (
                ProcessIdentity {
                    pid: 78,
                    started_seconds: 1,
                },
                77,
                vec![],
            ),
            (
                ProcessIdentity {
                    pid: 79,
                    started_seconds: 1,
                },
                77,
                vec![],
            ),
        ]);
        if let Some(descendant) = processes.rows.get_mut(&79) {
            descendant.parent = Some(78);
            descendant.group = Some(79);
        } else {
            return Err(Error::new(ErrorCode::State, "descendant fixture missing"));
        }
        assert_eq!(
            processes
                .command_memory_sample_with(
                    &ProcessIdentity {
                        pid: 78,
                        started_seconds: 1
                    },
                    crate::model::MemoryMetric::PssAnon,
                    |pid, _| anonymous_memory_bytes(root.path(), pid)
                )
                .map(|sample| sample.bytes),
            Some(204_800)
        );
        std::fs::write(
            root.path().join("78/smaps_rollup"),
            "Rss: 999999 kB\nPss_Anon: invalid kB\n",
        )?;
        assert_eq!(
            processes
                .command_memory_sample_with(
                    &ProcessIdentity {
                        pid: 78,
                        started_seconds: 1
                    },
                    crate::model::MemoryMetric::PssAnon,
                    |pid, _| anonymous_memory_bytes(root.path(), pid)
                )
                .map(|sample| sample.bytes),
            None
        );
        Ok(())
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
