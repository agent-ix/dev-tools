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

pub(crate) struct NamespaceSnapshot {
    namespaces: BTreeSet<String>,
    complete: bool,
}

impl NamespaceSnapshot {
    pub(crate) fn alive(&self, namespace: &str) -> Option<bool> {
        if self.namespaces.contains(namespace) {
            Some(true)
        } else if self.complete {
            Some(false)
        } else {
            None
        }
    }
}

#[cfg(test)]
fn namespace_alive_at(root: &std::path::Path, namespace: &str) -> Result<bool> {
    namespaces_at(root)?
        .alive(namespace)
        .ok_or_else(|| Error::new(ErrorCode::State, "PID namespace liveness is unknown"))
}

pub(crate) fn namespaces_at(root: &std::path::Path) -> Result<NamespaceSnapshot> {
    use std::os::unix::fs::{DirEntryExt, MetadataExt};
    // Every live foreign PID namespace has its original PID 1. Unlike ns/pid,
    // status/NSpid is readable for other users' non-dumpable host processes.
    // Host tasks and non-init members cannot hide a foreign namespace's init.
    // An init cannot be replaced: its exit kills the namespace's members.
    let mut incomplete = false;
    let mut namespaces = BTreeSet::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let listed_inode = entry.ino();
        let foreign_init = std::fs::read_to_string(entry.path().join("status"))
            .ok()
            .and_then(|status| {
                let values = status
                    .lines()
                    .find_map(|line| line.strip_prefix("NSpid:"))?
                    .split_whitespace()
                    .map(str::parse::<u32>)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .ok()?;
                (!values.is_empty() && values.iter().all(|pid| *pid > 0))
                    .then(|| values.len() > 1 && values.last() == Some(&1))
            });
        let gone = || {
            !entry.path().exists()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .parse::<i32>()
                    .is_ok_and(|pid| {
                        getpgid(Some(Pid::from_raw(pid))) == Err(nix::errno::Errno::ESRCH)
                    })
        };
        let observed = std::fs::read_link(entry.path().join("ns/pid"));
        match observed {
            Ok(value) => {
                namespaces.insert(value.to_string_lossy().into_owned());
            }
            Err(_) => {
                if foreign_init != Some(false) && !gone() {
                    incomplete = true;
                }
            }
        }
        match std::fs::metadata(entry.path()) {
            Ok(metadata) if metadata.ino() == listed_inode => {}
            _ => {
                if foreign_init != Some(false) && !gone() {
                    incomplete = true;
                }
            }
        }
    }
    Ok(NamespaceSnapshot {
        namespaces,
        complete: !incomplete,
    })
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
    pub(crate) fn self_identity() -> Result<ProcessIdentity> {
        let pid = sysinfo::Pid::from_u32(std::process::id());
        let mut system = System::new();
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::everything().without_tasks(),
        );
        let process = system
            .process(pid)
            .ok_or_else(|| Error::new(ErrorCode::State, "cannot verify launcher identity"))?;
        Ok(ProcessIdentity {
            pid: pid.as_u32(),
            started_seconds: process.start_time(),
        })
    }
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
        Self::from_system(system)
    }

    pub(crate) fn read_workers(pids: &[u32]) -> Self {
        let mut pids: Vec<_> = pids.iter().copied().map(sysinfo::Pid::from_u32).collect();
        pids.push(sysinfo::Pid::from_u32(std::process::id()));
        let mut system = System::new();
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::everything().without_tasks(),
        );
        Self::from_system(system)
    }

    fn from_system(system: System) -> Self {
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
    anonymous_memory_bytes_with(proc_root, pid, |path| std::fs::read_to_string(path))
}

fn anonymous_memory_bytes_with(
    proc_root: &std::path::Path,
    pid: u32,
    read_smaps: impl FnOnce(&std::path::Path) -> std::io::Result<String>,
) -> Option<u64> {
    let path = proc_root.join(pid.to_string()).join("smaps_rollup");
    let contents = match read_smaps(&path) {
        Ok(contents) => contents,
        Err(error) => {
            // smaps_rollup returns ESRCH when the task has no mm. During exit
            // or exec its PID/group can still exist and stat can still say R.
            if error.raw_os_error() == Some(nix::errno::Errno::ESRCH as i32) {
                return Some(0);
            }
            return exited_process_memory(proc_root, pid);
        }
    };
    parse_anonymous_memory(&contents).or_else(|| exited_process_memory(proc_root, pid))
}

fn exited_process_memory(proc_root: &std::path::Path, pid: u32) -> Option<u64> {
    exited_process_memory_with(proc_root, pid, |pid| {
        i32::try_from(pid).is_ok_and(|native| {
            getpgid(Some(Pid::from_raw(native))) == Err(nix::errno::Errno::ESRCH)
        })
    })
}

fn exited_process_memory_with(
    proc_root: &std::path::Path,
    pid: u32,
    is_gone: impl FnOnce(u32) -> bool,
) -> Option<u64> {
    if let Ok(stat) = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat"))
        && stat
            .rsplit_once(')')
            .and_then(|(_, tail)| tail.split_whitespace().next())
            .is_some_and(|state| matches!(state, "Z" | "X"))
    {
        return Some(0);
    }
    is_gone(pid).then_some(0)
}

fn parse_anonymous_memory(contents: &str) -> Option<u64> {
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

    /// Trace: FR-005-AC-4
    #[test]
    fn namespace_scan_distinguishes_live_extinct_and_unverifiable() -> Result<()> {
        let root = tempfile::tempdir()?;
        let ns = root.path().join("11/ns");
        std::fs::create_dir_all(&ns)?;
        std::os::unix::fs::symlink("pid:[host]", ns.join("pid"))?;
        assert!(namespace_alive_at(root.path(), "pid:[host]")?);
        assert!(!namespace_alive_at(root.path(), "pid:[extinct]")?);
        std::fs::create_dir(root.path().join("22"))?;
        assert!(namespace_alive_at(root.path(), "pid:[extinct]").is_err());
        assert!(namespace_alive_at(root.path(), "pid:[host]")?);
        Ok(())
    }

    /// Provenance: SWM-421. Trace: FR-005-AC-4
    #[test]
    fn host_tasks_without_readable_namespace_links_cannot_hide_a_foreign_namespace() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("11"))?;
        std::fs::write(
            root.path().join("11/status"),
            "Name:\thost-root\nNSpid:\t11\n",
        )?;
        std::fs::create_dir_all(root.path().join("22/ns"))?;
        std::fs::write(
            root.path().join("22/status"),
            "Name:\tnamespace-init\nNSpid:\t22\t1\n",
        )?;
        std::os::unix::fs::symlink("pid:[live]", root.path().join("22/ns/pid"))?;
        assert_eq!(namespaces_at(root.path())?.alive("pid:[dead]"), Some(false));
        assert_eq!(namespaces_at(root.path())?.alive("pid:[live]"), Some(true));
        std::fs::remove_file(root.path().join("22/ns/pid"))?;
        assert_eq!(namespaces_at(root.path())?.alive("pid:[dead]"), None);
        Ok(())
    }

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
        let parent_pid = std::process::id();
        let child_pid = u32::try_from(nix::unistd::getppid().as_raw())
            .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
        for pid in [parent_pid, child_pid] {
            let directory = root.path().join(pid.to_string());
            std::fs::create_dir(&directory)?;
            std::fs::write(
                directory.join("stat"),
                format!("{pid} (live fixture) R 77 77 77\n"),
            )?;
            std::fs::write(
                directory.join("smaps_rollup"),
                "Rss: 1000 kB\nPss: 600 kB\nPss_Anon: 100 kB\nPss_File: 200 kB\nPss_Shmem: 300 kB\n",
            )?;
        }
        let mut processes = Processes::test_snapshot([
            (
                ProcessIdentity {
                    pid: parent_pid,
                    started_seconds: 1,
                },
                77,
                vec![],
            ),
            (
                ProcessIdentity {
                    pid: child_pid,
                    started_seconds: 1,
                },
                77,
                vec![],
            ),
        ]);
        if let Some(descendant) = processes.rows.get_mut(&child_pid) {
            descendant.parent = Some(parent_pid);
            descendant.group = Some(child_pid);
        } else {
            return Err(Error::new(ErrorCode::State, "descendant fixture missing"));
        }
        assert_eq!(
            processes
                .command_memory_sample_with(
                    &ProcessIdentity {
                        pid: parent_pid,
                        started_seconds: 1
                    },
                    crate::model::MemoryMetric::PssAnon,
                    |pid, _| anonymous_memory_bytes(root.path(), pid)
                )
                .map(|sample| sample.bytes),
            Some(204_800)
        );
        std::fs::write(
            root.path()
                .join(parent_pid.to_string())
                .join("smaps_rollup"),
            "Rss: 999999 kB\nPss_Anon: invalid kB\n",
        )?;
        assert_eq!(
            processes
                .command_memory_sample_with(
                    &ProcessIdentity {
                        pid: parent_pid,
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

    /// Provenance: SWM-392. Trace: FR-005-AC-4
    #[test]
    fn child_becoming_a_zombie_during_memory_sampling_does_not_fail_live_cargo() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cargo = ProcessIdentity {
            pid: 78,
            started_seconds: 1,
        };
        // Use an existing PID so ESRCH alone cannot hide this race.
        let rustc = std::process::id();
        std::fs::create_dir(root.path().join("78"))?;
        std::fs::write(root.path().join("78/smaps_rollup"), "Pss_Anon: 100 kB\n")?;
        std::fs::create_dir(root.path().join(rustc.to_string()))?;
        std::fs::write(
            root.path().join(rustc.to_string()).join("stat"),
            format!("{rustc} (rustc) Z 78 77 77\n"),
        )?;
        let mut processes = Processes::test_snapshot([
            (cargo.clone(), 77, vec!["cargo".into()]),
            (
                ProcessIdentity {
                    pid: rustc,
                    started_seconds: 1,
                },
                77,
                vec!["rustc".into()],
            ),
        ]);
        processes
            .rows
            .get_mut(&rustc)
            .ok_or_else(|| Error::new(ErrorCode::State, "fixture child missing"))?
            .parent = Some(78);
        let sample = processes.command_memory_sample_with(
            &cargo,
            crate::model::MemoryMetric::PssAnon,
            |pid, _| anonymous_memory_bytes(root.path(), pid),
        );
        assert_eq!(sample.map(|sample| sample.bytes), Some(102_400));
        std::fs::write(
            root.path().join(rustc.to_string()).join("stat"),
            format!("{rustc} (rustc) R 78 77 77\n"),
        )?;
        assert_eq!(
            anonymous_memory_bytes(root.path(), rustc),
            None,
            "a live unreadable task must remain unknown"
        );
        Ok(())
    }

    /// Provenance: SWM-394, terra r244. Trace: FR-005-AC-4
    #[test]
    fn empty_smaps_from_an_exiting_child_is_not_lost_supervision() -> Result<()> {
        let root = tempfile::tempdir()?;
        let pid = std::process::id();
        let path = root.path().join(pid.to_string());
        std::fs::create_dir(&path)?;
        std::fs::write(
            path.join("stat"),
            format!("{pid} (compiler child) Z 78 77 77\n"),
        )?;
        std::fs::write(path.join("smaps_rollup"), "")?;
        assert_eq!(
            anonymous_memory_bytes(root.path(), pid),
            Some(0),
            "an empty successful read from a zombie must not invalidate its live parent's memory cap"
        );
        std::fs::write(
            path.join("stat"),
            format!("{pid} (compiler child) R 78 77 77\n"),
        )?;
        assert_eq!(
            anonymous_memory_bytes(root.path(), pid),
            None,
            "an unreadable live child must remain an explicit supervision failure"
        );
        Ok(())
    }

    /// Provenance: SWM-394, terra r244. Trace: FR-005-AC-4
    #[test]
    fn esrch_from_a_still_running_task_means_its_address_space_is_gone() -> Result<()> {
        let root = tempfile::tempdir()?;
        let pid = std::process::id();
        let path = root.path().join(pid.to_string());
        std::fs::create_dir(&path)?;
        // Native reproduction: the task remains R with vsize/RSS0 after
        // exiting its mm, before becoming Z or losing its PID/group.
        std::fs::write(path.join("stat"), format!("{pid} (true) R 78 77 77\n"))?;
        let value = anonymous_memory_bytes_with(root.path(), pid, |_| {
            Err(std::io::Error::from_raw_os_error(
                nix::errno::Errno::ESRCH as i32,
            ))
        });
        assert_eq!(value, Some(0));
        let denied = anonymous_memory_bytes_with(root.path(), pid, |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert_eq!(
            denied, None,
            "an unreadable live address space must fail closed"
        );
        Ok(())
    }

    /// Provenance: SWM-394, terra r244. Trace: FR-005-AC-4
    #[test]
    fn task_disappearing_after_a_running_stat_is_rechecked_by_the_kernel_probe() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("23");
        std::fs::create_dir(&path)?;
        std::fs::write(path.join("stat"), "23 (short lived) R 8 11 11\n")?;
        assert_eq!(
            exited_process_memory_with(root.path(), 23, |_| true),
            Some(0)
        );
        assert_eq!(exited_process_memory_with(root.path(), 23, |_| false), None);
        Ok(())
    }

    /// Provenance: SWM-394, terra r244. Trace: FR-005-AC-4
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "native Linux procfs/PID-namespace churn lane"]
    fn namespace_local_memory_sampling_survives_short_lived_children() -> Result<()> {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let root = Processes::self_identity()?;
        let done = Arc::new(AtomicBool::new(false));
        let finished = done.clone();
        let child_thread = std::thread::spawn(move || -> std::io::Result<()> {
            let result = (|| {
                for _ in 0..2000 {
                    std::process::Command::new("/usr/bin/true").status()?;
                }
                Ok(())
            })();
            finished.store(true, Ordering::Relaxed);
            result
        });
        let mut failed = None;
        let mut samples = 0;
        while !done.load(Ordering::Relaxed) {
            let processes = Processes::read();
            let sample =
                processes.command_memory_sample_with(&root, watchdog_memory_metric(), |pid, _| {
                    let value = anonymous_memory_bytes(std::path::Path::new("/proc"), pid);
                    if value.is_none() {
                        eprintln!(
                            "failed memory pid={pid} stat={:?} smaps={:?}",
                            std::fs::read_to_string(format!("/proc/{pid}/stat")),
                            std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
                        );
                    }
                    value
                });
            if sample.is_none() {
                failed = Some(format!(
                    "root={root:?}, observed={:?}",
                    processes.rows.get(&root.pid).map(|row| row.start)
                ));
                break;
            }
            samples += 1;
        }
        child_thread
            .join()
            .map_err(|_| Error::new(ErrorCode::State, "churn fixture panicked"))??;
        assert!(failed.is_none(), "{failed:?}");
        assert!(samples > 0);
        Ok(())
    }

    /// Provenance: SWM-392. Trace: FR-005-AC-4
    #[cfg(target_os = "linux")]
    #[test]
    fn kernel_zombie_smaps_error_does_not_abort_the_outer_cargo_tree() -> Result<()> {
        use std::process::{Command, Stdio};
        let mut child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;
        let processes = Processes::read();
        let root = processes
            .member_identity(std::process::id())
            .ok_or_else(|| Error::new(ErrorCode::State, "fixture parent missing"))?;
        assert!(processes.member_identity(child.id()).is_some());
        drop(child.stdin.take());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !owned_child_exited(&child)? {
            if std::time::Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                return Err(Error::new(
                    ErrorCode::State,
                    "fixture did not become a zombie",
                ));
            }
            std::thread::yield_now();
        }
        let sample = processes.command_memory_sample(&root);
        assert!(child.wait()?.success());
        assert!(
            sample.is_some(),
            "a dead compiler child must contribute zero, not terminate its live parent"
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
