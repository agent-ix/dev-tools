// SPDX-License-Identifier: MIT
//! Read-only host activity sampling for the metrics textfile collector.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) mod kache;

use crate::error::{Error, ErrorCode, Result};
use serde::Deserialize;

const CONFIG_LIMIT: u64 = 1_048_576;
const SOURCE_FILE_LIMIT: u64 = 1_048_576;
const PROCESS_LIMIT: usize = 16_384;
const PROCESS_LIST_BYTES_LIMIT: usize = PROCESS_LIMIT * 64;
const LOCKF_PROCESS_LIMIT: usize = 128;
const LOCKF_COMMAND_BYTES_LIMIT: usize = LOCKF_PROCESS_LIMIT * 262_144;
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
    cache: crate::config::CachePolicy,
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
    TerraWrapperFdinfo {
        proc_root: PathBuf,
        owner_uid: u32,
        wrapper_path: PathBuf,
        lane_paths: Vec<PathBuf>,
    },
    TitanBuildWrappersAndArchLocks {
        proc_root: PathBuf,
        owner_uid: u32,
        registry_path: PathBuf,
        arch_lock_prefix: PathBuf,
        arch_wrapper_path: PathBuf,
        wrapper_sources: Vec<WrapperLaneSource>,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WrapperLaneSource {
    wrapper_path: PathBuf,
    lane_paths: Vec<PathBuf>,
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

struct ProcessSummary {
    pid: u32,
    parent_pid: u32,
    state: char,
    command: String,
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

/// Read-only lease/process observations; no signaling or scheduler mutation.
pub(crate) fn lease_health(state: &crate::model::State) -> Option<(usize, usize)> {
    let namespace = crate::process::pid_namespace().ok()?;
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::everything().without_tasks()),
    );
    if system.processes().len() > PROCESS_LIMIT
        || system
            .process(sysinfo::Pid::from_u32(std::process::id()))
            .is_none()
    {
        return None;
    }
    let rows: Vec<_> = system
        .processes()
        .iter()
        .map(|(pid, p)| ProcessRecord {
            pid: pid.as_u32(),
            parent_pid: p.parent().map_or(0, |p| p.as_u32()),
            process_group_id: 0,
            start_time: p.start_time().to_string(),
            state: if p.status() == sysinfo::ProcessStatus::Zombie {
                'Z'
            } else {
                'S'
            },
            uid: None,
            arguments: p.cmd().to_vec(),
        })
        .collect();
    lease_health_from_processes(state, &rows, namespace.as_deref())
}
fn lease_health_from_processes(
    state: &crate::model::State,
    rows: &[ProcessRecord],
    namespace: Option<&str>,
) -> Option<(usize, usize)> {
    if state
        .requests
        .iter()
        .any(|r| r.phase == crate::model::Phase::Running && r.pid_namespace.as_deref() != namespace)
    {
        return None;
    }
    let by_pid: BTreeMap<_, _> = rows.iter().map(|row| (row.pid, row)).collect();
    let mut roots = BTreeSet::new();
    let mut missing = 0;
    for request in state
        .requests
        .iter()
        .filter(|r| r.phase == crate::model::Phase::Running)
    {
        let mut present = false;
        for identity in request.worker.iter().chain(
            request
                .lease_record
                .iter()
                .filter_map(|l| l.command.as_ref()),
        ) {
            if by_pid.get(&identity.pid).is_some_and(|r| {
                r.state != 'Z' && r.start_time == identity.started_seconds.to_string()
            }) {
                roots.insert(identity.pid);
                present = true;
            }
        }
        if !present {
            missing += 1;
        }
    }
    let unleased = rows
        .iter()
        .filter(|r| {
            let build = r
                .arguments
                .first()
                .and_then(|a| Path::new(a).file_name())
                .is_some_and(|name| name == "rustc" || name == "rustc-wrapper" || name == "cargo");
            if !build || r.state == 'Z' {
                return false;
            }
            let mut pid = r.pid;
            let mut visited = BTreeSet::new();
            while visited.insert(pid) {
                if roots.contains(&pid) {
                    return false;
                }
                match by_pid.get(&pid) {
                    Some(row) if row.parent_pid != 0 => pid = row.parent_pid,
                    _ => break,
                }
            }
            true
        })
        .count();
    Some((missing, unleased))
}

impl SamplerConfig {
    fn read(path: &Path) -> Result<Self> {
        let bytes = read_bounded(path, CONFIG_LIMIT).map_err(|error| {
            Error::new(
                ErrorCode::Configuration,
                format!("sampler config cannot be read safely: {}", error.kind()),
            )
        })?;
        let mut config: Self = serde_json::from_slice(&bytes)
            .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
        config.validate_cache_paths()?;
        config
            .cache
            .resolve_paths(path.parent().unwrap_or_else(|| Path::new(".")))?;
        config.validate()?;
        Ok(config)
    }

    fn validate_cache_paths(&self) -> Result<()> {
        for (name, path) in [
            ("cache.remote_report_file", &self.cache.remote_report_file),
            ("cache.remote_status_file", &self.cache.remote_status_file),
        ] {
            if let Some(path) = path {
                validate_absolute(path, name)?;
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        self.validate_cache_paths()?;
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
                | ("terra", BusySource::TerraWrapperFdinfo { .. })
                | ("titan", BusySource::TitanBuildWrappersAndArchLocks { .. })
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
            BusySource::TerraWrapperFdinfo {
                proc_root,
                wrapper_path,
                lane_paths,
                ..
            } => {
                validate_absolute(proc_root, "proc_root")?;
                validate_absolute(wrapper_path, "Terra wrapper path")?;
                validate_path_list(lane_paths, 12, "Terra lane paths")?;
            }
            BusySource::TitanBuildWrappersAndArchLocks {
                proc_root,
                registry_path,
                arch_lock_prefix,
                arch_wrapper_path,
                wrapper_sources,
                ..
            } => {
                validate_absolute(proc_root, "proc_root")?;
                validate_absolute(registry_path, "registry_path")?;
                validate_absolute(arch_lock_prefix, "Titan arch-test lock prefix")?;
                validate_absolute(arch_wrapper_path, "Titan arch-test wrapper path")?;
                if arch_wrapper_path.file_name() != Some(OsStr::new("arch-test-run.sh"))
                    || arch_wrapper_path
                        .parent()
                        .map(|home| home.join("dev/worktrees/arch-test-target-"))
                        != Some(arch_lock_prefix.clone())
                {
                    return Err(Error::new(
                        ErrorCode::Configuration,
                        "Titan arch lock source must match arch-test-run.sh's host worktrees",
                    ));
                }
                if arch_lock_prefix.file_name() != Some(OsStr::new("arch-test-target-")) {
                    return Err(Error::new(
                        ErrorCode::Configuration,
                        "Titan arch-test lock prefix must end with arch-test-target-",
                    ));
                }
                if wrapper_sources.len() != 2 {
                    return Err(Error::new(
                        ErrorCode::Configuration,
                        "Titan must configure both observed build wrappers",
                    ));
                }
                let mut wrapper_paths = BTreeSet::new();
                let mut lane_paths = BTreeSet::new();
                let mut has_ix_build = false;
                let mut has_cargo_slot = false;
                for wrapper in wrapper_sources {
                    validate_absolute(&wrapper.wrapper_path, "Titan wrapper path")?;
                    if !wrapper_paths.insert(&wrapper.wrapper_path)
                        || wrapper.wrapper_path == *arch_wrapper_path
                    {
                        return Err(Error::new(
                            ErrorCode::Configuration,
                            "Titan wrapper paths must be distinct",
                        ));
                    }
                    let expected_lane_paths =
                        match wrapper.wrapper_path.file_name().and_then(OsStr::to_str) {
                            Some("ix-build.sh") => {
                                has_ix_build = true;
                                titan_ix_build_lane_paths()
                            }
                            Some("cargo-slot.sh") => {
                                has_cargo_slot = true;
                                titan_cargo_slot_lane_paths(&wrapper.wrapper_path).ok_or_else(|| {
                                Error::new(
                                    ErrorCode::Configuration,
                                    "Titan cargo-slot.sh must have a parent worktree directory",
                                )
                            })?
                            }
                            _ => {
                                return Err(Error::new(
                                    ErrorCode::Configuration,
                                    "Titan wrapper path is not one of the observed build wrappers",
                                ));
                            }
                        };
                    validate_nonempty_path_list(&wrapper.lane_paths, "Titan wrapper lane paths")?;
                    if wrapper.lane_paths != expected_lane_paths {
                        return Err(Error::new(
                            ErrorCode::Configuration,
                            "Titan wrapper lanes do not match the observed source paths",
                        ));
                    }
                    for lane_path in &wrapper.lane_paths {
                        if !lane_paths.insert(lane_path) {
                            return Err(Error::new(
                                ErrorCode::Configuration,
                                "Titan wrapper lane paths must be distinct across sources",
                            ));
                        }
                    }
                }
                if !has_ix_build || !has_cargo_slot {
                    return Err(Error::new(
                        ErrorCode::Configuration,
                        "Titan must configure ix-build.sh and cargo-slot.sh",
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

fn titan_ix_build_lane_paths() -> Vec<PathBuf> {
    (1..=12)
        .map(|number| PathBuf::from(format!("/tmp/quire-heavy-check.lane{number}.lock")))
        .chain(std::iter::once(PathBuf::from(
            "/tmp/quire-heavy-check.lane-kani.lock",
        )))
        .collect()
}

fn titan_cargo_slot_lane_paths(wrapper_path: &Path) -> Option<Vec<PathBuf>> {
    let slots = wrapper_path.parent()?.join("cargo-slots");
    Some(
        (1..=3)
            .map(|number| slots.join(format!("slot{number}")))
            .collect(),
    )
}

pub(crate) fn run(config_path: &Path, output_path: &Path) -> Result<i32> {
    run_with_kache(config_path, output_path, kache::executable().as_deref())
}

fn run_with_kache(config_path: &Path, output_path: &Path, binary: Option<&Path>) -> Result<i32> {
    let config = SamplerConfig::read(config_path)?;
    let mut sampled = sample(&config);
    kache::append_with_binary(
        &mut sampled.exposition,
        &config.host,
        &output_path.with_extension("kache.json"),
        binary,
        &config.cache,
    );
    config
        .cache
        .append_info(&mut sampled.exposition, &config.host);
    for (source, reason) in &sampled.failures {
        eprintln!("ix-build sample: source={source} unavailable: {reason}");
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
            "ix_build_units_used",
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
            "ix_build_units_capacity",
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
                let host = crate::metrics::escape_label(&config.host);
                let repo = crate::metrics::escape_label(&target.repo);
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
        let host = crate::metrics::escape_label(&config.host);
        let probe = crate::metrics::escape_label(source);
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
    let host = crate::metrics::escape_label(host);
    output.push_str(&format!(
        "# HELP {name} {help}\n# TYPE {name} {kind}\n{name}{{host=\"{host}\"}} {value}\n"
    ));
}

fn busy_source_name(source: &BusySource) -> &'static str {
    match source {
        BusySource::MacosLockfChildren { .. } => "process_table",
        BusySource::TerraWrapperFdinfo { .. } => "wrapper_lane_ownership",
        BusySource::TitanBuildWrappersAndArchLocks { .. } => "controller_wrappers_and_arch_lanes",
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
        BusySource::TerraWrapperFdinfo {
            proc_root,
            owner_uid,
            wrapper_path,
            lane_paths,
        } => terra_wrapper_busy(proc_root, *owner_uid, wrapper_path, lane_paths),
        BusySource::TitanBuildWrappersAndArchLocks {
            proc_root,
            owner_uid,
            registry_path,
            arch_lock_prefix,
            arch_wrapper_path,
            wrapper_sources,
        } => titan_busy(
            proc_root,
            *owner_uid,
            registry_path,
            arch_lock_prefix,
            arch_wrapper_path,
            wrapper_sources,
        ),
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
    let arguments = vec![
        OsString::from("-axo"),
        OsString::from("pid=,ppid=,stat=,comm="),
    ];
    let output = bounded_command_output("/bin/ps", &arguments, PROCESS_LIST_BYTES_LIMIT)?;
    let summaries = parse_process_summaries(&output)?;
    let lockf_pids = summaries
        .iter()
        .filter(|summary| Path::new(&summary.command).file_name() == Some(OsStr::new("lockf")))
        .map(|summary| summary.pid)
        .collect::<Vec<_>>();
    let lockf_arguments = if lockf_pids.is_empty() {
        HashMap::new()
    } else {
        let pid_list = lockf_pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let arguments = vec![
            OsString::from("-ww"),
            OsString::from("-p"),
            OsString::from(pid_list),
            OsString::from("-o"),
            OsString::from("pid=,command="),
        ];
        let output = bounded_command_output("/bin/ps", &arguments, LOCKF_COMMAND_BYTES_LIMIT)?;
        parse_lockf_command_rows(&output, &lockf_pids)?
    };
    let records = summaries
        .into_iter()
        .map(|summary| ProcessRecord {
            pid: summary.pid,
            parent_pid: summary.parent_pid,
            process_group_id: 0,
            start_time: String::new(),
            state: summary.state,
            uid: None,
            arguments: lockf_arguments
                .get(&summary.pid)
                .cloned()
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let held = lockf_slot_paths(&records, lock_paths);
    u64::try_from(held.len()).map_err(|_| "held slot count overflow".to_owned())
}

fn bounded_command_output(
    program: &str,
    arguments: &[OsString],
    maximum_bytes: usize,
) -> std::result::Result<Vec<u8>, String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("process list command failed: {}", error.kind()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "process list output pipe is unavailable".to_owned())?;
    let maximum =
        u64::try_from(maximum_bytes).map_err(|_| "process list byte bound overflow".to_owned())?;
    let mut output = Vec::new();
    let read_result = (&mut stdout)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut output);
    // Close the pipe before waiting so an over-limit writer cannot block on a full pipe.
    drop(stdout);
    let status_result = child.wait();
    read_result.map_err(|error| format!("process list output read failed: {}", error.kind()))?;
    if output.len() > maximum_bytes {
        return Err("process list output byte limit exceeded".into());
    }
    let status = status_result
        .map_err(|error| format!("process list command wait failed: {}", error.kind()))?;
    if !status.success() {
        return Err("process list command returned failure".into());
    }
    Ok(output)
}

fn parse_process_summaries(output: &[u8]) -> std::result::Result<Vec<ProcessSummary>, String> {
    let output =
        std::str::from_utf8(output).map_err(|_| "process list output is not UTF-8".to_owned())?;
    let mut summaries = Vec::new();
    let mut seen = BTreeSet::new();
    let mut lockf_count = 0_usize;
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        if summaries.len() >= PROCESS_LIMIT {
            return Err("process table entry limit exceeded".into());
        }
        let mut fields = line.split_ascii_whitespace();
        let pid = fields
            .next()
            .ok_or_else(|| "process list PID is missing".to_owned())?
            .parse::<u32>()
            .map_err(|_| "process list PID is malformed".to_owned())?;
        let parent_pid = fields
            .next()
            .ok_or_else(|| "process list parent PID is missing".to_owned())?
            .parse::<u32>()
            .map_err(|_| "process list parent PID is malformed".to_owned())?;
        let state = fields
            .next()
            .and_then(|field| field.chars().next())
            .ok_or_else(|| "process list state is missing".to_owned())?;
        let command = fields.collect::<Vec<_>>().join(" ");
        if command.is_empty() || pid == 0 || !seen.insert(pid) {
            return Err("process list row is invalid".into());
        }
        if Path::new(&command).file_name() == Some(OsStr::new("lockf")) {
            lockf_count += 1;
            if lockf_count > LOCKF_PROCESS_LIMIT {
                return Err("lockf process candidate limit exceeded".into());
            }
        }
        summaries.push(ProcessSummary {
            pid,
            parent_pid,
            state,
            command,
        });
    }
    Ok(summaries)
}

fn parse_lockf_command_rows(
    output: &[u8],
    candidate_pids: &[u32],
) -> std::result::Result<HashMap<u32, Vec<OsString>>, String> {
    let output =
        std::str::from_utf8(output).map_err(|_| "lockf command output is not UTF-8".to_owned())?;
    let candidates = candidate_pids.iter().copied().collect::<BTreeSet<_>>();
    let mut commands = HashMap::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_ascii_whitespace();
        let pid = fields
            .next()
            .ok_or_else(|| "lockf command PID is missing".to_owned())?
            .parse::<u32>()
            .map_err(|_| "lockf command PID is malformed".to_owned())?;
        let arguments = fields.map(OsString::from).collect::<Vec<_>>();
        if arguments.is_empty() || !candidates.contains(&pid) || commands.contains_key(&pid) {
            return Err("lockf command row is invalid".into());
        }
        commands.insert(pid, arguments);
    }
    Ok(commands)
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
    arch_wrapper_path: &Path,
    wrapper_sources: &[WrapperLaneSource],
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
            active_process_groups.insert(process.process_group_id);
        }
    }
    for wrapper in wrapper_sources {
        let lane_paths = wrapper.lane_paths.iter().cloned().collect::<BTreeSet<_>>();
        let wrapper_groups =
            wrapper_lane_holder_groups(proc_root, owner_uid, &wrapper.wrapper_path, |target| {
                lane_paths.contains(target)
            })?;
        active_process_groups.extend(
            wrapper_groups
                .values()
                .flat_map(|groups| groups.iter().copied()),
        );
    }
    let arch_lane_groups =
        wrapper_lane_holder_groups(proc_root, owner_uid, arch_wrapper_path, |target| {
            is_arch_test_lock(target, arch_lock_prefix)
        })?;
    active_process_groups.extend(
        arch_lane_groups
            .values()
            .flat_map(|groups| groups.iter().copied()),
    );
    u64::try_from(active_process_groups.len())
        .map_err(|_| "active build process-group count overflow".into())
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
        if process.state == 'Z' {
            continue;
        }
        let Some(subcommand) = cargo_subcommand(&process.arguments, cargo_command_name) else {
            continue;
        };
        if build_test_commands
            .iter()
            .any(|command| command == subcommand)
        {
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

pub(crate) fn cargo_subcommand<'a>(
    arguments: &'a [OsString],
    command_name: &str,
) -> Option<&'a str> {
    if !argv0_matches(arguments.first(), command_name) {
        return None;
    }
    let mut index = 1;
    while let Some(argument) = arguments.get(index).and_then(|argument| argument.to_str()) {
        if index == 1 && argument.starts_with('+') && argument.len() > 1 {
            index += 1;
            continue;
        }
        if matches!(
            argument,
            "-h" | "--help" | "-V" | "--version" | "--list" | "--explain"
        ) || argument.starts_with("--explain=")
        {
            return None;
        }
        if matches!(
            argument,
            "--color" | "--config" | "-Z" | "-C" | "--directory" | "--jobs" | "-j"
        ) {
            index += 2;
            continue;
        }
        if matches!(
            argument,
            "--locked" | "--offline" | "--frozen" | "--verbose" | "-v" | "--quiet" | "-q"
        ) || argument.starts_with("--color=")
            || argument.starts_with("--config=")
            || argument.starts_with("-Z")
            || argument.starts_with("-C")
            || argument.starts_with("--directory=")
            || argument.starts_with("--jobs=")
            || argument.starts_with("-j")
        {
            index += 1;
            continue;
        }
        if argument.starts_with('-') {
            return None;
        }
        return Some(argument);
    }
    None
}

fn terra_wrapper_busy(
    proc_root: &Path,
    owner_uid: u32,
    wrapper_path: &Path,
    lock_paths: &[PathBuf],
) -> std::result::Result<u64, String> {
    let lanes = terra_wrapper_lock_paths(proc_root, owner_uid, wrapper_path, lock_paths)?;
    u64::try_from(lanes.len()).map_err(|_| "lock lane count overflow".into())
}

fn terra_wrapper_lock_paths(
    proc_root: &Path,
    owner_uid: u32,
    wrapper_path: &Path,
    lock_paths: &[PathBuf],
) -> std::result::Result<BTreeSet<PathBuf>, String> {
    let lane_groups = wrapper_lane_holder_groups(proc_root, owner_uid, wrapper_path, |target| {
        lock_paths.iter().any(|path| path == target)
    })?;
    Ok(lane_groups.into_keys().collect())
}

fn wrapper_lane_holder_groups(
    proc_root: &Path,
    owner_uid: u32,
    wrapper_path: &Path,
    matches_target: impl Fn(&Path) -> bool,
) -> std::result::Result<BTreeMap<PathBuf, BTreeSet<u32>>, String> {
    let entries = fs::read_dir(proc_root)
        .map_err(|error| format!("process table enumeration failed: {}", error.kind()))?;
    let mut pids_seen = 0_usize;
    let mut total_source_bytes = 0_usize;
    let mut total_fd_entries = 0_usize;
    let mut total_fdinfo_entries = 0_usize;
    let mut total_fdinfo_bytes = 0_usize;
    let mut active_lanes = BTreeMap::<PathBuf, BTreeSet<u32>>::new();

    for entry in entries {
        let entry =
            entry.map_err(|error| format!("process table enumeration failed: {}", error.kind()))?;
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
        let metadata = match fs::metadata(&process_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("process metadata read failed: {}", error.kind())),
        };
        if !metadata.is_dir() || metadata.uid() != owner_uid {
            continue;
        }

        let stat_path = process_dir.join("stat");
        let stat = match read_bounded(&stat_path, 16_384) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("process stat read failed: {}", error.kind())),
        };
        add_process_bytes(&mut total_source_bytes, stat.len())?;
        if !proc_stat_comm_is(&stat, b"bash")? {
            continue;
        }
        let (_, process_group_id, _, state) = parse_proc_stat(&stat)?;
        if state == 'Z' {
            continue;
        }

        let cmdline = match read_bounded(&process_dir.join("cmdline"), 65_536) {
            Ok(cmdline) => cmdline,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("wrapper cmdline read failed: {}", error.kind())),
        };
        add_process_bytes(&mut total_source_bytes, cmdline.len())?;
        let arguments = cmdline
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| OsString::from_vec(argument.to_vec()))
            .collect::<Vec<_>>();
        if arguments.is_empty() {
            return Err("bash cmdline is empty during wrapper enumeration".into());
        }
        if !arguments
            .get(1)
            .is_some_and(|argument| argument == wrapper_path.as_os_str())
        {
            continue;
        }

        let fd_dir = process_dir.join("fd");
        let entries = match fs::read_dir(&fd_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("wrapper fd enumeration failed: {}", error.kind())),
        };
        let mut process_fd_entries = 0_usize;
        for fd_entry in entries {
            let fd_entry = fd_entry
                .map_err(|error| format!("wrapper fd enumeration failed: {}", error.kind()))?;
            process_fd_entries += 1;
            total_fd_entries += 1;
            if process_fd_entries > FD_LIMIT_PER_PROCESS || total_fd_entries > FDINFO_ENTRY_LIMIT {
                return Err("wrapper fd entry limit exceeded".into());
            }
            let fd_name = fd_entry.file_name();
            let descriptor_target = match fs::read_link(fd_entry.path()) {
                Ok(target) => target,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!("wrapper fd target read failed: {}", error.kind()));
                }
            };
            if !matches_target(&descriptor_target) {
                continue;
            }
            let fdinfo_path = process_dir.join("fdinfo").join(fd_name);
            let fdinfo = match read_bounded(&fdinfo_path, 16_384) {
                Ok(fdinfo) => fdinfo,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!("wrapper fdinfo read failed: {}", error.kind()));
                }
            };
            total_fdinfo_entries += 1;
            total_fdinfo_bytes = total_fdinfo_bytes
                .checked_add(fdinfo.len())
                .ok_or_else(|| "wrapper fdinfo byte count overflow".to_owned())?;
            check_fdinfo_bounds(total_fdinfo_entries, total_fdinfo_bytes)?;
            if has_flock_write_lock(&fdinfo) {
                active_lanes
                    .entry(descriptor_target)
                    .or_default()
                    .insert(process_group_id);
            }
        }
    }
    Ok(active_lanes)
}

fn proc_stat_comm_is(stat: &[u8], expected: &[u8]) -> std::result::Result<bool, String> {
    let start = stat
        .iter()
        .position(|byte| *byte == b'(')
        .ok_or_else(|| "process stat has no command start".to_owned())?;
    let end = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or_else(|| "process stat has no command terminator".to_owned())?;
    if end <= start {
        return Err("process stat command field is malformed".into());
    }
    Ok(&stat[start + 1..end] == expected)
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
            cache: crate::config::CachePolicy::default(),
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
        assert!(!result.exposition.contains("ix_build_units_capacity"));
        assert!(!result.exposition.contains("ix_build_units_used"));
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

    /// Trace: FR-015-AC-15
    #[test]
    fn macos_process_summary_parser_enforces_process_limit() {
        let mut rows = String::new();
        for pid in 1..=PROCESS_LIMIT + 1 {
            rows.push_str(&format!("{pid} 1 S cargo\n"));
        }
        assert!(parse_process_summaries(rows.as_bytes()).is_err());
        assert_eq!(
            parse_process_summaries(b"1 0 S launchd\n2 1 S lockf\n3 1 S Google Chrome\n")
                .map(|rows| rows.len()),
            Ok(3)
        );
        let mut lockf_rows = String::new();
        for pid in 1..=LOCKF_PROCESS_LIMIT + 1 {
            lockf_rows.push_str(&format!("{pid} 1 S lockf\n"));
        }
        assert!(parse_process_summaries(lockf_rows.as_bytes()).is_err());
        assert_eq!(
            parse_lockf_command_rows(b"2 /usr/bin/lockf -t 0 /tmp/qsl-slot.lock /bin/sh\n", &[2],)
                .map(|commands| commands.len()),
            Ok(1)
        );
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
                    "kind": "terra_wrapper_fdinfo",
                    "proc_root": root.path().join("missing-proc"),
                    "owner_uid": 1000,
                    "wrapper_path": "/home/builder/dev/worktrees/ix-build.sh",
                    "lane_paths": lane_paths
                }
            }))?,
        )?;

        assert_eq!(run_with_kache(&config_path, &output_path, None)?, 0);
        let exposition = fs::read_to_string(&output_path)?;
        assert!(exposition.contains("ix_build_units_capacity{host=\"terra\"} 12"));
        assert!(!exposition.contains("ix_build_units_used"));
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
        proc_entry_for_uid(
            root,
            pid,
            parent_pid,
            group_id,
            start_time,
            (state, 1000),
            arguments,
        )
    }

    fn proc_entry_for_uid(
        root: &Path,
        pid: u32,
        parent_pid: u32,
        group_id: u32,
        start_time: u64,
        state_uid: (char, u32),
        arguments: &[&str],
    ) -> std::io::Result<PathBuf> {
        let (state, uid) = state_uid;
        let path = root.join(pid.to_string());
        fs::create_dir_all(&path)?;
        fs::write(
            path.join("status"),
            format!("Name:\ttest\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
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
        let comm = arguments
            .first()
            .and_then(|argument| Path::new(argument).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "test-process".into());
        fs::write(
            path.join("stat"),
            format!("{pid} ({comm}) {}\n", stat_fields.join(" ")),
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
    fn terra_counts_admitted_paths_but_not_queued_or_unrelated_wrappers() -> TestResult {
        let root = tempfile::tempdir()?;
        let wrapper = PathBuf::from("/home/builder/dev/worktrees/ix-build.sh");
        let lane_one = PathBuf::from("/tmp/quire-heavy-check.lane1.lock");
        let lane_two = PathBuf::from("/tmp/quire-heavy-check.lane2.lock");
        let lane_three = PathBuf::from("/tmp/quire-heavy-check.lane3.lock");
        let process = proc_entry(
            root.path(),
            101,
            1,
            101,
            300,
            'S',
            &["bash", "/home/builder/dev/worktrees/ix-build.sh", "build"],
        )?;
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
        let inherited = proc_entry(
            root.path(),
            102,
            101,
            101,
            301,
            'S',
            &["bash", "/home/builder/dev/worktrees/ix-build.sh", "test"],
        )?;
        fs::write(
            inherited.join("fdinfo/6"),
            "pos:\t0\nflags:\t0100002\nlock:\t1: FLOCK ADVISORY WRITE 102 00:33:123 END\n",
        )?;
        std::os::unix::fs::symlink(&lane_one, inherited.join("fd/6"))?;
        let queued = proc_entry(
            root.path(),
            103,
            1,
            103,
            302,
            'S',
            &["bash", "/home/builder/dev/worktrees/ix-build.sh", "build"],
        )?;
        fs::write(queued.join("fdinfo/7"), "pos:\t0\nflags:\t0100002\n")?;
        std::os::unix::fs::symlink(&lane_three, queued.join("fd/7"))?;
        let unrelated = proc_entry(
            root.path(),
            104,
            1,
            104,
            303,
            'S',
            &["bash", "/home/builder/dev/worktrees/other.sh", "build"],
        )?;
        fs::write(
            unrelated.join("fdinfo/8"),
            "pos:\t0\nflags:\t0100002\nlock:\t1: FLOCK ADVISORY WRITE 104 00:33:125 END\n",
        )?;
        std::os::unix::fs::symlink(&lane_three, unrelated.join("fd/8"))?;

        let owner_uid = fs::metadata(root.path())?.uid();
        let active = terra_wrapper_lock_paths(
            root.path(),
            owner_uid,
            &wrapper,
            &[lane_one.clone(), lane_two.clone(), lane_three.clone()],
        )?;
        assert_eq!(active, BTreeSet::from([lane_one, lane_two]));
        assert!(
            terra_wrapper_lock_paths(
                root.path(),
                owner_uid.wrapping_add(1),
                &wrapper,
                &[lane_three],
            )?
            .is_empty()
        );
        Ok(())
    }

    /// Trace: FR-015-AC-15
    #[test]
    fn titan_counts_controller_wrapper_and_arch_sources_once_per_process_group() -> TestResult {
        let root = tempfile::tempdir()?;
        let prefix = PathBuf::from("/home/builder/dev/worktrees/arch-test-target-");
        let arch_wrapper = PathBuf::from("/home/builder/arch-test-run.sh");
        let build_wrapper = PathBuf::from("/home/builder/dev/worktrees/ix-build.sh");
        let cargo_slot_wrapper =
            PathBuf::from("/home/builder/dev/worktrees/QSL-handoff/cargo-slot.sh");
        let build_lane = PathBuf::from("/tmp/quire-heavy-check.lane1.lock");
        let cargo_slot_lane =
            PathBuf::from("/home/builder/dev/worktrees/QSL-handoff/cargo-slots/slot1");
        let arch_lane = PathBuf::from("/home/builder/dev/worktrees/arch-test-target-repo.lock");
        let another_arch_lane =
            PathBuf::from("/home/builder/dev/worktrees/arch-test-target-repo-2.lock");
        let owner_uid = fs::metadata(root.path())?.uid();
        let process = proc_entry_for_uid(
            root.path(),
            201,
            1,
            201,
            901,
            ('S', owner_uid),
            &["bash", "/home/builder/dev/worktrees/ix-build.sh", "build"],
        )?;
        fs::write(
            process.join("fdinfo/9"),
            "lock:\t1: FLOCK ADVISORY WRITE 201 00:33:456 END\n",
        )?;
        fs::write(
            process.join("fdinfo/10"),
            "lock:\t1: FLOCK ADVISORY WRITE 201 00:33:457 END\n",
        )?;
        std::os::unix::fs::symlink(&build_lane, process.join("fd/9"))?;
        std::os::unix::fs::symlink(&build_lane, process.join("fd/10"))?;
        let inherited = proc_entry_for_uid(
            root.path(),
            202,
            201,
            201,
            902,
            ('S', owner_uid),
            &["bash", "/home/builder/dev/worktrees/ix-build.sh", "build"],
        )?;
        fs::write(
            inherited.join("fdinfo/11"),
            "lock:\t1: FLOCK ADVISORY WRITE 202 00:33:456 END\n",
        )?;
        std::os::unix::fs::symlink(&build_lane, inherited.join("fd/11"))?;
        let arch_holder = proc_entry_for_uid(
            root.path(),
            203,
            201,
            201,
            903,
            ('S', owner_uid),
            &["bash", "/home/builder/arch-test-run.sh", "repo"],
        )?;
        fs::write(
            arch_holder.join("fdinfo/12"),
            "lock:\t1: FLOCK ADVISORY WRITE 203 00:33:458 END\n",
        )?;
        std::os::unix::fs::symlink(&arch_lane, arch_holder.join("fd/12"))?;
        let independent_arch = proc_entry_for_uid(
            root.path(),
            303,
            1,
            303,
            904,
            ('S', owner_uid),
            &["bash", "/home/builder/arch-test-run.sh", "repo"],
        )?;
        fs::write(
            independent_arch.join("fdinfo/13"),
            "lock:\t1: FLOCK ADVISORY WRITE 303 00:33:459 END\n",
        )?;
        std::os::unix::fs::symlink(&another_arch_lane, independent_arch.join("fd/13"))?;
        let cargo_slot = proc_entry_for_uid(
            root.path(),
            404,
            1,
            404,
            905,
            ('S', owner_uid),
            &[
                "bash",
                "/home/builder/dev/worktrees/QSL-handoff/cargo-slot.sh",
                "log",
            ],
        )?;
        fs::write(
            cargo_slot.join("fdinfo/14"),
            "lock:\t1: FLOCK ADVISORY WRITE 404 00:33:460 END\n",
        )?;
        std::os::unix::fs::symlink(&cargo_slot_lane, cargo_slot.join("fd/14"))?;
        let registry = root.path().join("jobs.json");
        fs::write(
            &registry,
            serde_json::to_vec(&serde_json::json!({
                "sequence": 1,
                "jobs": [{
                    "pid": 201, "pgid": 201, "start": "901", "sequence": 1,
                    "owner": owner_uid, "command": ["cargo", "build"], "launched": "2026-11-25T00:00:00Z"
                }]
            }))?,
        )?;
        let wrapper_sources = vec![
            WrapperLaneSource {
                wrapper_path: build_wrapper,
                lane_paths: titan_ix_build_lane_paths(),
            },
            WrapperLaneSource {
                lane_paths: titan_cargo_slot_lane_paths(&cargo_slot_wrapper)
                    .ok_or("Titan cargo-slot fixture must have a worktree parent")?,
                wrapper_path: cargo_slot_wrapper,
            },
        ];
        let config = SamplerConfig {
            schema_version: 1,
            host: "titan".into(),
            cache: crate::config::CachePolicy::default(),
            capacity_units: None,
            busy_source: BusySource::TitanBuildWrappersAndArchLocks {
                proc_root: root.path().to_path_buf(),
                owner_uid,
                registry_path: registry,
                arch_lock_prefix: prefix,
                arch_wrapper_path: arch_wrapper,
                wrapper_sources,
            },
            target_dirs: Vec::new(),
        };
        config.validate()?;
        let sampled = sample(&config);
        assert!(
            sampled
                .exposition
                .contains("ix_build_units_used{host=\"titan\"} 3")
        );
        assert!(!sampled.exposition.contains("ix_build_units_capacity"));
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
        proc_entry(
            root.path(),
            501,
            1,
            501,
            4,
            'S',
            &["cargo", "help", "build"],
        )?;
        proc_entry(root.path(), 601, 1, 601, 5, 'S', &["cargo", "new", "build"])?;
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

    /// Trace: FR-015-AC-15
    #[test]
    fn cargo_subcommand_parser_uses_command_position_and_global_options() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            cargo_subcommand(&args(&["cargo", "help", "build"]), "cargo"),
            Some("help")
        );
        assert_eq!(
            cargo_subcommand(&args(&["cargo", "new", "build"]), "cargo"),
            Some("new")
        );
        assert_eq!(
            cargo_subcommand(
                &args(&[
                    "cargo",
                    "+stable",
                    "--config",
                    "net.offline=true",
                    "--color",
                    "always",
                    "build",
                ]),
                "cargo"
            ),
            Some("build")
        );
        assert_eq!(cargo_subcommand(&args(&["cargo", "--help"]), "cargo"), None);
        assert_eq!(
            cargo_subcommand(&args(&["cargo", "--explain", "E0001", "build"]), "cargo"),
            None
        );
    }

    /// Trace: FR-015-AC-15
    #[test]
    fn cargo_help_and_new_commands_do_not_count_as_build_roots() -> TestResult {
        let root = tempfile::tempdir()?;
        proc_entry(
            root.path(),
            701,
            1,
            701,
            7,
            'S',
            &["cargo", "help", "build"],
        )?;
        proc_entry(root.path(), 702, 1, 702, 8, 'S', &["cargo", "new", "build"])?;
        assert_eq!(
            cargo_tree_busy(
                root.path(),
                1000,
                "cargo",
                &["build".into(), "test".into(), "check".into()],
            )?,
            0
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
            cache: crate::config::CachePolicy::default(),
            capacity_units: Some(12),
            busy_source: BusySource::TerraWrapperFdinfo {
                proc_root: root.path().join("missing-proc"),
                owner_uid: 1000,
                wrapper_path: PathBuf::from("/home/builder/dev/worktrees/ix-build.sh"),
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
                .contains("ix_build_units_capacity{host=\"terra\"} 12")
        );
        assert!(
            result
                .exposition
                .contains("ix_build_target_dir_bytes{host=\"terra\",repo=\"sample\"} 13")
        );
        assert!(result.exposition.contains(
            "ix_build_probe_unavailable{host=\"terra\",probe=\"wrapper_lane_ownership\"} 1"
        ));
        assert!(!result.exposition.contains("ix_build_units_used"));
        assert_eq!(result.failures.len(), 1);
        Ok(())
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    use crate::metrics::tests::{request, state};
    use crate::model::{LeaseRecord, Phase, ProcessIdentity};
    /// Trace: FR-015-AC-15
    #[test]
    fn telemetry_unleased_compilers_exclude_verified_lease_descendants_and_reused_pids() {
        let mut running = request(1, "draco:tooling:one".into(), Phase::Running);
        running.lease_record = Some(LeaseRecord {
            command: Some(ProcessIdentity {
                pid: 10,
                started_seconds: 7,
            }),
            ..Default::default()
        });
        let mut missing = request(2, "draco:tooling:two".into(), Phase::Running);
        missing.worker = Some(ProcessIdentity {
            pid: 40,
            started_seconds: 8,
        });
        let row = |pid, parent, start: u64, command: &str| ProcessRecord {
            pid,
            parent_pid: parent,
            process_group_id: pid,
            start_time: start.to_string(),
            state: 'S',
            uid: None,
            arguments: vec![command.into()],
        };
        let rows = vec![
            row(10, 1, 7, "cargo"),
            row(11, 10, 7, "rustc"),
            row(20, 1, 7, "rustc"),
            row(30, 1, 7, "rustc-wrapper"),
            row(40, 1, 9, "rustc"),
        ];
        assert_eq!(
            lease_health_from_processes(&state(vec![running, missing]), &rows, None),
            Some((1, 3))
        );
    }
    /// Trace: FR-015-AC-15
    #[test]
    fn telemetry_foreign_or_unknown_namespace_is_not_reported_as_local_process_health() {
        let mut running = request(1, "draco:tooling:one".into(), Phase::Running);
        running.pid_namespace = Some("pid:[11]".into());
        running.worker = Some(ProcessIdentity {
            pid: 10,
            started_seconds: 7,
        });
        let rows = vec![ProcessRecord {
            pid: 10,
            parent_pid: 1,
            process_group_id: 10,
            start_time: "7".into(),
            state: 'S',
            uid: None,
            arguments: vec!["rustc".into()],
        }];
        assert_eq!(
            lease_health_from_processes(&state(vec![running.clone()]), &rows, Some("pid:[11]")),
            Some((0, 0))
        );
        assert_eq!(
            lease_health_from_processes(&state(vec![running.clone()]), &rows, Some("pid:[22]")),
            None
        );
        running.pid_namespace = None;
        assert_eq!(
            lease_health_from_processes(&state(vec![running]), &rows, Some("pid:[22]")),
            None
        );
    }
}

#[cfg(test)]
mod cache_path_tests {
    use super::*;
    /// Provenance: SWM-409 delta review; standalone snapshot files must be absolute at input.
    #[test]
    fn sample_rejects_relative_remote_snapshot_paths()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("sampler.json");
        for key in ["remote_report_file", "remote_status_file"] {
            fs::write(
                &path,
                serde_json::to_vec(
                    &serde_json::json!({"schema_version":1,"host":"luna","busy_source":{"kind":"cargo_process_trees","proc_root":"/proc","owner_uid":1000,"cargo_command_name":"cargo","build_test_commands":["build"]},"cache":{(key):"relative.json"}}),
                )?,
            )?;
            let error = SamplerConfig::read(&path)
                .err()
                .ok_or("relative path accepted")?;
            assert!(error.to_string().contains(key));
            assert!(error.to_string().contains("absolute"));
        }
        Ok(())
    }
}
