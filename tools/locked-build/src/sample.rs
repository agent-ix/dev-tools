// SPDX-License-Identifier: MIT
//! Read-only host activity sampling for the metrics textfile collector.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

use crate::error::{Error, ErrorCode, Result};

const CONFIG_LIMIT: u64 = 1_048_576;
const SOURCE_FILE_LIMIT: u64 = 1_048_576;
const PROCESS_LIMIT: usize = 16_384;
const PROCESS_SOURCE_BYTES_LIMIT: usize = 64 * 1024 * 1024;
const FD_LIMIT_PER_PROCESS: usize = 4096;
const FDINFO_ENTRY_LIMIT: usize = 131_072;
const FDINFO_BYTES_LIMIT: usize = 64 * 1024 * 1024;
const DIRECTORY_ENTRY_LIMIT: usize = 100_000;
const DIRECTORY_DEPTH_LIMIT: usize = 64;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SamplerConfig {
    schema_version: u32,
    host: String,
    #[serde(default)]
    capacity_units: Option<u32>,
    busy_source: BusySource,
    #[serde(default)]
    target_dirs: Vec<TargetDirectory>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum BusySource {
    MacosLockfChildren {
        lock_paths: Vec<PathBuf>,
    },
    LinuxFdinfoLockPaths {
        proc_root: PathBuf,
        owner_uid: u32,
        lane_paths: Vec<PathBuf>,
    },
    TitanControllerJobsAndArchLocks {
        proc_root: PathBuf,
        owner_uid: u32,
        registry_path: PathBuf,
        arch_lock_prefix: PathBuf,
    },
    CargoProcessTrees {
        proc_root: PathBuf,
        owner_uid: u32,
        cargo_command_name: String,
        build_test_commands: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetDirectory {
    repo: String,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct ProcessRecord {
    pid: u32,
    parent_pid: u32,
    process_group_id: u32,
    start_time: String,
    state: char,
    uid: Option<u32>,
    arguments: Vec<OsString>,
}

#[derive(Deserialize)]
struct ControllerRegistry {
    sequence: u64,
    jobs: Vec<ControllerJob>,
}

#[derive(Deserialize)]
struct ControllerJob {
    pid: u32,
    pgid: u32,
    start: String,
    sequence: u64,
    owner: u32,
    command: Vec<String>,
    launched: String,
    #[serde(default)]
    stopping: Option<u64>,
    #[serde(default)]
    killed: Option<bool>,
    #[serde(default)]
    reason: Option<String>,
}

struct SampleResult {
    exposition: String,
    failures: BTreeMap<&'static str, String>,
}

impl SamplerConfig {
    fn read(path: &Path) -> Result<Self> {
        let bytes = read_bounded(path, CONFIG_LIMIT).map_err(|error| {
            Error::new(
                ErrorCode::Configuration,
                format!("sampler config cannot be read safely: {}", error.kind()),
            )
        })?;
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.host.is_empty() || self.host.len() > 128 {
            return Err(Error::new(
                ErrorCode::Configuration,
                "invalid sampler schema version or host label",
            ));
        }
        let expected = match self.host.as_str() {
            "draco" => Some(6),
            "terra" => Some(12),
            "titan" | "luna" => None,
            _ => {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "sampler source mapping is not defined for this host",
                ));
            }
        };
        if self.capacity_units != expected {
            return Err(Error::new(
                ErrorCode::Configuration,
                "configured sampler capacity does not match its sourced host value",
            ));
        }
        let source_matches = matches!(
            (&self.host[..], &self.busy_source),
            ("draco", BusySource::MacosLockfChildren { .. })
                | ("terra", BusySource::LinuxFdinfoLockPaths { .. })
                | ("titan", BusySource::TitanControllerJobsAndArchLocks { .. })
                | ("luna", BusySource::CargoProcessTrees { .. })
        );
        if !source_matches {
            return Err(Error::new(
                ErrorCode::Configuration,
                "sampler busy source does not match its host adapter",
            ));
        }
        match &self.busy_source {
            BusySource::MacosLockfChildren { lock_paths } => {
                validate_path_list(lock_paths, 6, "Draco lock paths")?;
            }
            BusySource::LinuxFdinfoLockPaths {
                proc_root,
                lane_paths,
                ..
            } => {
                validate_absolute(proc_root, "proc_root")?;
                validate_path_list(lane_paths, 12, "Terra lane paths")?;
            }
            BusySource::TitanControllerJobsAndArchLocks {
                proc_root,
                registry_path,
                arch_lock_prefix,
                ..
            } => {
                validate_absolute(proc_root, "proc_root")?;
                validate_absolute(registry_path, "registry_path")?;
                validate_absolute(arch_lock_prefix, "Titan arch-test lock prefix")?;
                if arch_lock_prefix.file_name() != Some(OsStr::new("arch-test-target-")) {
                    return Err(Error::new(
                        ErrorCode::Configuration,
                        "Titan arch-test lock prefix must end with arch-test-target-",
                    ));
                }
            }
            BusySource::CargoProcessTrees {
                proc_root,
                cargo_command_name,
                build_test_commands,
                ..
            } => {
                validate_absolute(proc_root, "proc_root")?;
                if cargo_command_name != "cargo" {
                    return Err(Error::new(
                        ErrorCode::Configuration,
                        "Cargo process selector must match the observed cargo command name",
                    ));
                }
                if build_test_commands.is_empty()
                    || build_test_commands.len() > 8
                    || build_test_commands
                        .iter()
                        .any(|name| !matches!(name.as_str(), "build" | "test" | "check"))
                {
                    return Err(Error::new(
                        ErrorCode::Configuration,
                        "invalid Cargo build/test command selectors",
                    ));
                }
            }
        }
        if self.target_dirs.len() > 64 {
            return Err(Error::new(
                ErrorCode::Configuration,
                "too many configured target directories",
            ));
        }
        let mut repos = BTreeSet::new();
        for target in &self.target_dirs {
            if target.repo.is_empty()
                || target.repo.len() > 64
                || !target
                    .repo
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
                || !repos.insert(target.repo.as_str())
            {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "invalid or duplicate target directory repo label",
                ));
            }
            validate_absolute(&target.path, "target directory")?;
        }
        Ok(())
    }
}

fn validate_absolute(path: &Path, name: &str) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::new(
            ErrorCode::Configuration,
            format!("{name} must be an absolute path"),
        ));
    }
    Ok(())
}

fn validate_path_list(paths: &[PathBuf], expected: usize, name: &str) -> Result<()> {
    if paths.len() != expected {
        return Err(Error::new(
            ErrorCode::Configuration,
            format!("{name} must contain exactly {expected} entries"),
        ));
    }
    validate_nonempty_path_list(paths, name)
}

fn validate_nonempty_path_list(paths: &[PathBuf], name: &str) -> Result<()> {
    if paths.is_empty() || paths.len() > 128 {
        return Err(Error::new(
            ErrorCode::Configuration,
            format!("{name} has an invalid entry count"),
        ));
    }
    let mut distinct = BTreeSet::new();
    for path in paths {
        validate_absolute(path, name)?;
        if !distinct.insert(path) {
            return Err(Error::new(
                ErrorCode::Configuration,
                format!("{name} contains duplicate entries"),
            ));
        }
    }
    Ok(())
}

pub(crate) fn run(config_path: &Path, output_path: &Path) -> Result<i32> {
    let config = SamplerConfig::read(config_path)?;
    let sampled = sample(&config);
    for (source, reason) in &sampled.failures {
        eprintln!("locked-build sample: source={source} unavailable: {reason}");
    }
    crate::metrics::write_atomic(output_path, &sampled.exposition)?;
    Ok(0)
}

fn sample(config: &SamplerConfig) -> SampleResult {
    let mut output = String::new();
    let mut failures = BTreeMap::new();
    match busy_count(&config.busy_source) {
        Ok(busy) => metric(
            &mut output,
            "ix_build_slots_busy",
            "gauge",
            "Currently active build processes counted by the configured host source.",
            &config.host,
            busy,
        ),
        Err(reason) => {
            failures.insert(
                busy_source_name(&config.busy_source),
                bounded_reason(reason),
            );
        }
    }
    if let Some(capacity) = config.capacity_units {
        metric(
            &mut output,
            "ix_build_slots_configured",
            "gauge",
            "Configured ordinary build slots from the sourced host lane count.",
            &config.host,
            capacity,
        );
    }
    let mut target_values = Vec::new();
    for target in &config.target_dirs {
        match directory_bytes(&target.path) {
            Ok(bytes) => {
                let host = escape_label(&config.host);
                let repo = escape_label(&target.repo);
                target_values.push(format!(
                    "ix_build_target_dir_bytes{{host=\"{host}\",repo=\"{repo}\"}} {bytes}\n"
                ));
            }
            Err(reason) => {
                failures
                    .entry("target_dir")
                    .or_insert_with(|| bounded_reason(reason));
            }
        }
    }
    if !target_values.is_empty() {
        output.push_str(
            "# HELP ix_build_target_dir_bytes Bytes in configured regular files under a target directory.\n# TYPE ix_build_target_dir_bytes gauge\n",
        );
        for value in target_values {
            output.push_str(&value);
        }
    }
    for source in failures.keys() {
        let host = escape_label(&config.host);
        let probe = escape_label(source);
        if !output.contains("# TYPE ix_build_probe_unavailable gauge\n") {
            output.push_str(
                "# HELP ix_build_probe_unavailable A configured sample source could not be read.\n# TYPE ix_build_probe_unavailable gauge\n",
            );
        }
        output.push_str(&format!(
            "ix_build_probe_unavailable{{host=\"{host}\",probe=\"{probe}\"}} 1\n"
        ));
    }
    SampleResult {
        exposition: output,
        failures,
    }
}

fn metric<T: std::fmt::Display>(
    output: &mut String,
    name: &str,
    kind: &str,
    help: &str,
    host: &str,
    value: T,
) {
    let host = escape_label(host);
    output.push_str(&format!(
        "# HELP {name} {help}\n# TYPE {name} {kind}\n{name}{{host=\"{host}\"}} {value}\n"
    ));
}

fn busy_source_name(source: &BusySource) -> &'static str {
    match source {
        BusySource::MacosLockfChildren { .. } => "process_table",
        BusySource::LinuxFdinfoLockPaths { .. } => "lock_ownership",
        BusySource::TitanControllerJobsAndArchLocks { .. } => "controller_and_arch_locks",
        BusySource::CargoProcessTrees { .. } => "cargo_processes",
    }
}

fn bounded_reason(reason: String) -> String {
    let mut bounded = reason
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect::<String>();
    if bounded.is_empty() {
        bounded.push_str("source read failed");
    }
    bounded
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn directory_bytes(path: &Path) -> std::result::Result<u64, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("configured target is not a regular directory".into());
    }
    let mut stack = vec![(path.to_path_buf(), 0_usize)];
    let mut entries_seen = 0_usize;
    let mut total = 0_u64;
    while let Some((directory, depth)) = stack.pop() {
        if depth > DIRECTORY_DEPTH_LIMIT {
            return Err("target directory depth limit exceeded".into());
        }
        let entries = fs::read_dir(&directory).map_err(|error| error.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            entries_seen = entries_seen
                .checked_add(1)
                .ok_or_else(|| "target directory entry count overflow".to_owned())?;
            if entries_seen > DIRECTORY_ENTRY_LIMIT {
                return Err("target directory entry limit exceeded".into());
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    "target changed during measurement".to_owned()
                } else {
                    error.to_string()
                }
            })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                stack.push((entry.path(), depth + 1));
            } else if metadata.is_file() {
                total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| "target directory byte count overflow".to_owned())?;
            }
        }
    }
    Ok(total)
}

fn busy_count(source: &BusySource) -> std::result::Result<u64, String> {
    match source {
        BusySource::MacosLockfChildren { lock_paths } => macos_lockf_busy(lock_paths),
        BusySource::LinuxFdinfoLockPaths {
            proc_root,
            owner_uid,
            lane_paths,
        } => fdinfo_lock_holders(proc_root, *owner_uid, lane_paths),
        BusySource::TitanControllerJobsAndArchLocks {
            proc_root,
            owner_uid,
            registry_path,
            arch_lock_prefix,
        } => titan_busy(proc_root, *owner_uid, registry_path, arch_lock_prefix),
        BusySource::CargoProcessTrees {
            proc_root,
            owner_uid,
            cargo_command_name,
            build_test_commands,
        } => cargo_tree_busy(
            proc_root,
            *owner_uid,
            cargo_command_name,
            build_test_commands,
        ),
    }
}

fn macos_lockf_busy(lock_paths: &[PathBuf]) -> std::result::Result<u64, String> {
    let mut system = System::new_with_specifics(RefreshKind::nothing());
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );
    let records = system
        .processes()
        .iter()
        .map(|(pid, process)| ProcessRecord {
            pid: pid.as_u32(),
            parent_pid: process.parent().map_or(0, sysinfo::Pid::as_u32),
            process_group_id: 0,
            start_time: process.start_time().to_string(),
            state: if process.status() == sysinfo::ProcessStatus::Zombie {
                'Z'
            } else {
                'R'
            },
            uid: None,
            arguments: process.cmd().to_vec(),
        })
        .collect::<Vec<_>>();
    let held = lockf_slot_paths(&records, lock_paths);
    u64::try_from(held.len()).map_err(|_| "held slot count overflow".to_owned())
}

fn lockf_slot_paths(processes: &[ProcessRecord], lock_paths: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut held = BTreeSet::new();
    for process in processes {
        if process.state == 'Z'
            || process.arguments.first() != Some(&OsString::from("/usr/bin/lockf"))
        {
            continue;
        }
        for path in lock_paths {
            let has_slot_argument = process.arguments.windows(3).any(|window| {
                window[0] == OsStr::new("-t")
                    && window[1] == OsStr::new("0")
                    && window[2] == path.as_os_str()
            });
            if has_slot_argument
                && processes
                    .iter()
                    .any(|child| child.parent_pid == process.pid && child.state != 'Z')
            {
                held.insert(path.clone());
            }
        }
    }
    held
}

fn titan_busy(
    proc_root: &Path,
    owner_uid: u32,
    registry_path: &Path,
    arch_lock_prefix: &Path,
) -> std::result::Result<u64, String> {
    let bytes = read_bounded(registry_path, SOURCE_FILE_LIMIT)
        .map_err(|error| format!("controller registry read failed: {}", error.kind()))?;
    let registry: ControllerRegistry =
        serde_json::from_slice(&bytes).map_err(|_| "controller registry is invalid".to_owned())?;
    let processes = process_table(proc_root, owner_uid, false)?;
    let by_pid = processes
        .iter()
        .map(|process| (process.pid, process))
        .collect::<HashMap<_, _>>();
    let mut active = BTreeSet::new();
    let mut active_process_groups = BTreeSet::new();
    for job in registry.jobs {
        if job.pid == 0
            || job.pgid != job.pid
            || job.sequence == 0
            || job.sequence > registry.sequence
            || job.owner == 0
            || job.command.is_empty()
            || job.launched.is_empty()
        {
            return Err("controller registry contains an invalid job identity".into());
        }
        let _terminal_shedding = (job.stopping, job.killed, job.reason);
        if let Some(process) = by_pid.get(&job.pid)
            && process.uid == Some(owner_uid)
            && process.process_group_id == job.pgid
            && process.start_time == job.start
            && process.state != 'Z'
        {
            active.insert(job.pid);
            active_process_groups.insert(process.process_group_id);
        }
    }
    let arch_lanes = fdinfo_lock_holder_groups(proc_root, &processes, |target| {
        is_arch_test_lock(target, arch_lock_prefix)
    })?;
    let independent_arch_lanes = arch_lanes
        .values()
        .filter(|groups| {
            !groups
                .iter()
                .any(|group| active_process_groups.contains(group))
        })
        .count();
    u64::try_from(active.len() + independent_arch_lanes)
        .map_err(|_| "active job and arch-lane count overflow".into())
}

fn cargo_tree_busy(
    proc_root: &Path,
    owner_uid: u32,
    cargo_command_name: &str,
    build_test_commands: &[String],
) -> std::result::Result<u64, String> {
    let processes = process_table(proc_root, owner_uid, true)?;
    let by_pid = processes
        .iter()
        .map(|process| (process.pid, process))
        .collect::<HashMap<_, _>>();
    let mut cargo_builds = BTreeSet::new();
    for process in &processes {
        if process.state == 'Z' || !argv0_matches(process.arguments.first(), cargo_command_name) {
            continue;
        }
        if process.arguments.iter().skip(1).any(|argument| {
            argument
                .to_str()
                .is_some_and(|argument| build_test_commands.iter().any(|cmd| cmd == argument))
        }) {
            cargo_builds.insert(process.pid);
        }
    }
    let mut roots = BTreeSet::new();
    for pid in &cargo_builds {
        let mut parent = by_pid.get(pid).map(|process| process.parent_pid);
        let mut traversed = 0_usize;
        let mut nested = false;
        while let Some(parent_pid) = parent {
            if parent_pid == 0 || traversed >= PROCESS_LIMIT {
                break;
            }
            if cargo_builds.contains(&parent_pid) {
                nested = true;
                break;
            }
            parent = by_pid.get(&parent_pid).map(|process| process.parent_pid);
            traversed += 1;
        }
        if !nested {
            roots.insert(*pid);
        }
    }
    u64::try_from(roots.len()).map_err(|_| "Cargo tree count overflow".into())
}

fn argv0_matches(argument: Option<&OsString>, command_name: &str) -> bool {
    argument
        .and_then(|argument| Path::new(argument).file_name())
        .is_some_and(|name| name == OsStr::new(command_name))
}

fn fdinfo_lock_holders(
    proc_root: &Path,
    owner_uid: u32,
    lock_paths: &[PathBuf],
) -> std::result::Result<u64, String> {
    let processes = process_table(proc_root, owner_uid, false)?;
    let lanes = fdinfo_lock_holder_groups(proc_root, &processes, |target| {
        lock_paths.iter().any(|path| path == target)
    })?;
    u64::try_from(lanes.len()).map_err(|_| "lock lane count overflow".into())
}

fn fdinfo_lock_holder_groups(
    proc_root: &Path,
    processes: &[ProcessRecord],
    matches_target: impl Fn(&Path) -> bool,
) -> std::result::Result<BTreeMap<PathBuf, BTreeSet<u32>>, String> {
    let mut lane_holders = BTreeMap::<PathBuf, BTreeSet<u32>>::new();
    let mut total_fdinfo_entries = 0_usize;
    let mut total_fdinfo_bytes = 0_usize;
    for process in processes.iter().filter(|process| process.state != 'Z') {
        let process_dir = proc_root.join(process.pid.to_string());
        let fdinfo_dir = process_dir.join("fdinfo");
        let fd_dir = process_dir.join("fd");
        let entries = match fs::read_dir(&fdinfo_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("fdinfo enumeration failed: {}", error.kind())),
        };
        let mut fd_count = 0_usize;
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("fdinfo enumeration failed: {}", error.kind()))?;
            fd_count += 1;
            if fd_count > FD_LIMIT_PER_PROCESS {
                return Err("process fdinfo entry limit exceeded".into());
            }
            total_fdinfo_entries += 1;
            if total_fdinfo_entries > FDINFO_ENTRY_LIMIT {
                return Err("total fdinfo entry limit exceeded".into());
            }
            let fd_name = entry.file_name();
            let info_path = entry.path();
            let bytes = match read_bounded(&info_path, 16_384) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("fdinfo read failed: {}", error.kind())),
            };
            total_fdinfo_bytes = total_fdinfo_bytes
                .checked_add(bytes.len())
                .ok_or_else(|| "total fdinfo byte count overflow".to_owned())?;
            check_fdinfo_bounds(total_fdinfo_entries, total_fdinfo_bytes)?;
            if !has_flock_write_lock(&bytes) {
                continue;
            }
            let descriptor = fd_dir.join(fd_name);
            let descriptor_target = match fs::read_link(&descriptor) {
                Ok(target) => target,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("fd target read failed: {}", error.kind())),
            };
            if matches_target(&descriptor_target) {
                lane_holders
                    .entry(descriptor_target)
                    .or_default()
                    .insert(process.process_group_id);
            }
        }
    }
    Ok(lane_holders)
}

fn is_arch_test_lock(target: &Path, prefix: &Path) -> bool {
    let Some(target) = target.to_str() else {
        return false;
    };
    let Some(prefix) = prefix.to_str() else {
        return false;
    };
    let Some(name) = target.strip_prefix(prefix) else {
        return false;
    };
    let Some(name) = name.strip_suffix(".lock") else {
        return false;
    };
    let repo = ["-2", "-3", "-4"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name);
    let mut characters = repo.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

fn has_flock_write_lock(fdinfo: &[u8]) -> bool {
    fdinfo.split(|byte| *byte == b'\n').any(|line| {
        let fields = line
            .split(|byte| byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        fields.first().is_some_and(|field| *field == b"lock:")
            && fields.iter().any(|field| *field == b"FLOCK")
            && fields.iter().any(|field| *field == b"WRITE")
    })
}

fn check_fdinfo_bounds(entries: usize, bytes: usize) -> std::result::Result<(), String> {
    if entries > FDINFO_ENTRY_LIMIT {
        return Err("total fdinfo entry limit exceeded".into());
    }
    if bytes > FDINFO_BYTES_LIMIT {
        return Err("total fdinfo byte limit exceeded".into());
    }
    Ok(())
}

fn process_table(
    proc_root: &Path,
    owner_uid: u32,
    include_command: bool,
) -> std::result::Result<Vec<ProcessRecord>, String> {
    let entries = fs::read_dir(proc_root).map_err(|error| error.to_string())?;
    let mut processes = Vec::new();
    let mut pids_seen = 0_usize;
    let mut total_bytes = 0_usize;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        pids_seen += 1;
        if pids_seen > PROCESS_LIMIT {
            return Err("process table entry limit exceeded".into());
        }
        let process_dir = proc_root.join(pid.to_string());
        let status = match read_bounded(&process_dir.join("status"), 16_384) {
            Ok(status) => status,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("process status read failed: {}", error.kind())),
        };
        add_process_bytes(&mut total_bytes, status.len())?;
        let uid = parse_uid(&status)?;
        if uid != owner_uid {
            continue;
        }
        let stat = match read_bounded(&process_dir.join("stat"), 16_384) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("process stat read failed: {}", error.kind())),
        };
        add_process_bytes(&mut total_bytes, stat.len())?;
        let (parent_pid, process_group_id, start_time, state) = parse_proc_stat(&stat)?;
        let mut arguments = Vec::new();
        if include_command {
            let cmdline = match read_bounded(&process_dir.join("cmdline"), 65_536) {
                Ok(cmdline) => cmdline,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("process command read failed: {}", error.kind())),
            };
            add_process_bytes(&mut total_bytes, cmdline.len())?;
            arguments = cmdline
                .split(|byte| *byte == 0)
                .filter(|argument| !argument.is_empty())
                .map(|argument| OsString::from_vec(argument.to_vec()))
                .collect();
        }
        processes.push(ProcessRecord {
            pid,
            parent_pid,
            process_group_id,
            start_time,
            state,
            uid: Some(uid),
            arguments,
        });
    }
    Ok(processes)
}

fn add_process_bytes(total: &mut usize, bytes: usize) -> std::result::Result<(), String> {
    *total = total
        .checked_add(bytes)
        .ok_or_else(|| "process source byte count overflow".to_owned())?;
    if *total > PROCESS_SOURCE_BYTES_LIMIT {
        return Err("process source byte limit exceeded".into());
    }
    Ok(())
}

fn parse_uid(status: &[u8]) -> std::result::Result<u32, String> {
    let line = status
        .split(|byte| *byte == b'\n')
        .find(|line| line.starts_with(b"Uid:"))
        .ok_or_else(|| "process status has no uid".to_owned())?;
    let uid = line
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .nth(1)
        .ok_or_else(|| "process status uid is malformed".to_owned())?;
    std::str::from_utf8(uid)
        .map_err(|_| "process status uid is malformed".to_owned())?
        .parse::<u32>()
        .map_err(|_| "process status uid is malformed".to_owned())
}

fn parse_proc_stat(stat: &[u8]) -> std::result::Result<(u32, u32, String, char), String> {
    let stat = std::str::from_utf8(stat).map_err(|_| "process stat is not UTF-8".to_owned())?;
    let command_end = stat
        .rfind(')')
        .ok_or_else(|| "process stat has no command terminator".to_owned())?;
    let fields = stat
        .get(command_end + 1..)
        .ok_or_else(|| "process stat fields are missing".to_owned())?
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    let state = fields
        .first()
        .and_then(|field| field.chars().next())
        .ok_or_else(|| "process stat state is missing".to_owned())?;
    let parse_field = |index: usize| -> std::result::Result<u32, String> {
        fields
            .get(index)
            .ok_or_else(|| "process stat field is missing".to_owned())?
            .parse::<u32>()
            .map_err(|_| "process stat field is malformed".to_owned())
    };
    let parent_pid = parse_field(1)?;
    let process_group_id = parse_field(2)?;
    let start_time = fields
        .get(19)
        .ok_or_else(|| "process stat start time is missing".to_owned())?
        .to_string();
    Ok((parent_pid, process_group_id, start_time, state))
}

fn read_bounded(path: &Path, maximum: u64) -> std::io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "source is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "source exceeds configured read bound",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn process(
        pid: u32,
        parent_pid: u32,
        process_group_id: u32,
        state: char,
        arguments: &[&str],
    ) -> ProcessRecord {
        ProcessRecord {
            pid,
            parent_pid,
            process_group_id,
            start_time: "77".into(),
            state,
            uid: Some(1000),
            arguments: arguments.iter().map(OsString::from).collect(),
        }
    }

    /// Trace: FR-015-AC-15, FR-015-AC-17
    #[test]
    fn uncapped_host_omits_configured_slots_when_busy_source_is_unavailable() {
        let config = SamplerConfig {
            schema_version: 1,
            host: "luna".into(),
            capacity_units: None,
            busy_source: BusySource::CargoProcessTrees {
                proc_root: PathBuf::from("/missing/proc"),
                owner_uid: 1000,
                cargo_command_name: "cargo".into(),
                build_test_commands: vec!["build".into(), "test".into(), "check".into()],
            },
            target_dirs: Vec::new(),
        };

        let result = sample(&config);
        assert!(!result.exposition.contains("ix_build_slots_configured"));
        assert!(!result.exposition.contains("ix_build_slots_busy"));
        assert!(result.exposition.contains("ix_build_probe_unavailable"));
    }

    /// Trace: FR-015-AC-17
    #[test]
    fn source_enumeration_budgets_reject_aggregate_overflow() {
        assert!(check_fdinfo_bounds(FDINFO_ENTRY_LIMIT, FDINFO_BYTES_LIMIT).is_ok());
        assert!(check_fdinfo_bounds(FDINFO_ENTRY_LIMIT + 1, 0).is_err());
        assert!(check_fdinfo_bounds(0, FDINFO_BYTES_LIMIT + 1).is_err());
        let mut process_bytes = 0;
        assert!(add_process_bytes(&mut process_bytes, PROCESS_SOURCE_BYTES_LIMIT).is_ok());
        assert!(add_process_bytes(&mut process_bytes, 1).is_err());
    }

    /// Trace: FR-015-AC-13
    #[test]
    fn sample_writes_only_the_requested_output_without_scheduler_state() -> TestResult {
        let root = tempfile::tempdir()?;
        let config_path = root.path().join("sampler.json");
        let output_path = root.path().join("ix_build_metrics.prom");
        let lane_paths = (1..=12)
            .map(|number| format!("/tmp/quire-heavy-check.lane{number}.lock"))
            .collect::<Vec<_>>();
        fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "host": "terra",
                "capacity_units": 12,
                "busy_source": {
                    "kind": "linux_fdinfo_lock_paths",
                    "proc_root": root.path().join("missing-proc"),
                    "owner_uid": 1000,
                    "lane_paths": lane_paths
                }
            }))?,
        )?;

        assert_eq!(run(&config_path, &output_path)?, 0);
        let exposition = fs::read_to_string(&output_path)?;
        assert!(exposition.contains("ix_build_slots_configured{host=\"terra\"} 12"));
        assert!(!exposition.contains("ix_build_slots_busy"));
        assert!(exposition.contains("ix_build_probe_unavailable"));
        let mut files = fs::read_dir(root.path())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        files.sort();
        assert_eq!(
            files,
            vec![
                OsString::from("ix_build_metrics.prom"),
                OsString::from("sampler.json")
            ]
        );
        Ok(())
    }

    fn proc_entry(
        root: &Path,
        pid: u32,
        parent_pid: u32,
        group_id: u32,
        start_time: u64,
        state: char,
        arguments: &[&str],
    ) -> std::io::Result<PathBuf> {
        let path = root.join(pid.to_string());
        fs::create_dir_all(&path)?;
        fs::write(
            path.join("status"),
            "Name:\ttest\nUid:\t1000\t1000\t1000\t1000\n",
        )?;
        let mut stat_fields = vec![
            state.to_string(),
            parent_pid.to_string(),
            group_id.to_string(),
        ];
        while stat_fields.len() < 19 {
            stat_fields.push("0".into());
        }
        stat_fields.push(start_time.to_string());
        fs::write(
            path.join("stat"),
            format!("{pid} (test process) {}\n", stat_fields.join(" ")),
        )?;
        let mut cmdline = Vec::new();
        for argument in arguments {
            cmdline.extend_from_slice(argument.as_bytes());
            cmdline.push(0);
        }
        fs::write(path.join("cmdline"), cmdline)?;
        fs::create_dir_all(path.join("fdinfo"))?;
        fs::create_dir_all(path.join("fd"))?;
        Ok(path)
    }

    /// Trace: FR-015-AC-15
    #[test]
    fn draco_lockf_counts_distinct_paths_only_when_process_has_child() {
        let lane_one = PathBuf::from("/tmp/qsl-slot-1.lock");
        let lane_two = PathBuf::from("/tmp/qsl-slot-2.lock");
        let processes = vec![
            process(
                10,
                1,
                10,
                'S',
                &[
                    "/usr/bin/lockf",
                    "-t",
                    "0",
                    "/tmp/qsl-slot-1.lock",
                    "/bin/sh",
                ],
            ),
            process(11, 10, 10, 'S', &["/bin/sh"]),
            process(
                20,
                1,
                20,
                'S',
                &[
                    "/usr/bin/lockf",
                    "-t",
                    "0",
                    "/tmp/qsl-slot-2.lock",
                    "/bin/sh",
                ],
            ),
            process(
                30,
                1,
                30,
                'S',
                &[
                    "/usr/bin/lockf",
                    "-t",
                    "0",
                    "/tmp/qsl-slot-1.lock",
                    "/bin/sh",
                ],
            ),
            process(31, 30, 30, 'Z', &["/bin/sh"]),
        ];
        assert_eq!(lockf_slot_paths(&processes, &[lane_one, lane_two]).len(), 1);
    }

    /// Trace: FR-015-AC-15
    #[test]
    fn fdinfo_counts_distinct_lane_paths_across_inherited_descriptors() -> TestResult {
        let root = tempfile::tempdir()?;
        let lane_one = PathBuf::from("/tmp/nonexistent-lane-one.lock");
        let lane_two = PathBuf::from("/tmp/nonexistent-lane-two.lock");
        let process = proc_entry(root.path(), 101, 1, 101, 300, 'S', &[])?;
        fs::write(
            process.join("fdinfo/4"),
            "pos:\t0\nflags:\t0100002\nlock:\t1: FLOCK ADVISORY WRITE 101 00:33:123 END\n",
        )?;
        fs::write(
            process.join("fdinfo/5"),
            "pos:\t0\nflags:\t0100002\nlock:\t1: FLOCK ADVISORY WRITE 101 00:33:124 END\n",
        )?;
        std::os::unix::fs::symlink(&lane_one, process.join("fd/4"))?;
        std::os::unix::fs::symlink(&lane_two, process.join("fd/5"))?;
        let inherited = proc_entry(root.path(), 102, 101, 101, 301, 'S', &[])?;
        fs::write(
            inherited.join("fdinfo/6"),
            "pos:\t0\nflags:\t0100002\nlock:\t1: FLOCK ADVISORY WRITE 102 00:33:123 END\n",
        )?;
        std::os::unix::fs::symlink(&lane_one, inherited.join("fd/6"))?;

        assert_eq!(
            fdinfo_lock_holders(root.path(), 1000, &[lane_one, lane_two])?,
            2
        );
        Ok(())
    }

    /// Trace: FR-015-AC-15
    #[test]
    fn titan_controller_and_lane_sources_deduplicate_same_live_process() -> TestResult {
        let root = tempfile::tempdir()?;
        let prefix = PathBuf::from("/home/peter/dev/worktrees/arch-test-target-");
        let lane = PathBuf::from("/home/peter/dev/worktrees/arch-test-target-unseen-repo.lock");
        let another_lane =
            PathBuf::from("/home/peter/dev/worktrees/arch-test-target-unseen-repo-2.lock");
        let process = proc_entry(root.path(), 201, 1, 201, 901, 'S', &[])?;
        fs::write(
            process.join("fdinfo/9"),
            "lock:\t1: FLOCK ADVISORY WRITE 201 00:33:456 END\n",
        )?;
        std::os::unix::fs::symlink(&lane, process.join("fd/9"))?;
        fs::write(
            process.join("fdinfo/10"),
            "lock:\t1: FLOCK ADVISORY WRITE 201 00:33:457 END\n",
        )?;
        std::os::unix::fs::symlink(&another_lane, process.join("fd/10"))?;
        let inherited = proc_entry(root.path(), 202, 201, 201, 902, 'S', &[])?;
        fs::write(
            inherited.join("fdinfo/11"),
            "lock:\t1: FLOCK ADVISORY WRITE 202 00:33:456 END\n",
        )?;
        std::os::unix::fs::symlink(&lane, inherited.join("fd/11"))?;
        let unadmitted = proc_entry(root.path(), 303, 1, 303, 903, 'S', &[])?;
        let independent_lane =
            PathBuf::from("/home/peter/dev/worktrees/arch-test-target-another-repo-4.lock");
        fs::write(
            unadmitted.join("fdinfo/12"),
            "lock:\t1: FLOCK ADVISORY WRITE 303 00:33:458 END\n",
        )?;
        std::os::unix::fs::symlink(&independent_lane, unadmitted.join("fd/12"))?;
        let registry = root.path().join("jobs.json");
        fs::write(
            &registry,
            serde_json::to_vec(&serde_json::json!({
                "sequence": 1,
                "jobs": [{
                    "pid": 201, "pgid": 201, "start": "901", "sequence": 1,
                    "owner": 80, "command": ["cargo", "build"], "launched": "2026-11-25T00:00:00Z"
                }]
            }))?,
        )?;
        assert_eq!(titan_busy(root.path(), 1000, &registry, &prefix)?, 2);
        Ok(())
    }

    /// Trace: FR-015-AC-15
    #[test]
    fn luna_cargo_build_and_test_trees_count_only_top_level_commands() -> TestResult {
        let root = tempfile::tempdir()?;
        proc_entry(
            root.path(),
            301,
            1,
            301,
            1,
            'S',
            &["cargo", "test", "--workspace"],
        )?;
        proc_entry(
            root.path(),
            302,
            301,
            301,
            2,
            'S',
            &["cargo", "build", "-p", "helper"],
        )?;
        proc_entry(
            root.path(),
            401,
            1,
            401,
            3,
            'S',
            &["cargo", "build", "-p", "other"],
        )?;
        assert_eq!(
            cargo_tree_busy(
                root.path(),
                1000,
                "cargo",
                &["build".into(), "test".into(), "check".into()],
            )?,
            2
        );
        Ok(())
    }

    /// Trace: FR-015-AC-16
    #[test]
    fn target_directory_bytes_skip_symlinks_and_total_regular_files() -> TestResult {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        fs::write(root.path().join("first.bin"), [0_u8; 4])?;
        fs::create_dir(root.path().join("nested"))?;
        fs::write(root.path().join("nested/second.bin"), [0_u8; 7])?;
        fs::write(outside.path().join("private.bin"), [0_u8; 99])?;
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked"))?;
        assert_eq!(directory_bytes(root.path())?, 11);
        Ok(())
    }

    /// Trace: FR-015-AC-17, FR-015-AC-18
    #[test]
    fn unavailable_busy_source_keeps_configured_and_target_metrics() -> TestResult {
        let root = tempfile::tempdir()?;
        let target = root.path().join("target");
        fs::create_dir(&target)?;
        fs::write(target.join("artifact"), [0_u8; 13])?;
        let config = SamplerConfig {
            schema_version: 1,
            host: "terra".into(),
            capacity_units: Some(12),
            busy_source: BusySource::LinuxFdinfoLockPaths {
                proc_root: root.path().join("missing-proc"),
                owner_uid: 1000,
                lane_paths: (1..=12)
                    .map(|lane| PathBuf::from(format!("/tmp/lane{lane}.lock")))
                    .collect(),
            },
            target_dirs: vec![TargetDirectory {
                repo: "sample".into(),
                path: target,
            }],
        };
        let result = sample(&config);
        assert!(
            result
                .exposition
                .contains("ix_build_slots_configured{host=\"terra\"} 12")
        );
        assert!(
            result
                .exposition
                .contains("ix_build_target_dir_bytes{host=\"terra\",repo=\"sample\"} 13")
        );
        assert!(
            result
                .exposition
                .contains("ix_build_probe_unavailable{host=\"terra\",probe=\"lock_ownership\"} 1")
        );
        assert!(!result.exposition.contains("ix_build_slots_busy"));
        assert_eq!(result.failures.len(), 1);
        Ok(())
    }
}
