// SPDX-License-Identifier: MIT
//! Idempotent host setup and pinned tool-version checks.

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};

const TOOL_MANIFEST: &str = include_str!("../tool-versions.json");
const MANIFEST_VERSION: u32 = 1;
const NPM_REGISTRY: &str = "https://registry.npmjs.org/";
const NPM_SCOPE_REGISTRY: &str = "--@agent-ix:registry=https://registry.npmjs.org/";

#[path = "install/transaction.rs"]
mod transaction;

pub(crate) fn recover_transaction(store: &crate::store::Store) -> Result<()> {
    transaction::recover(store)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ToolManifest {
    schema_version: u32,
    tools: Vec<ToolPin>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ToolPin {
    name: String,
    executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    npm_package: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct InstalledRecord {
    schema_version: u32,
    #[serde(alias = "locked_build_version")]
    ix_build_version: String,
    policy_path: PathBuf,
    cargo_shim: PathBuf,
    pinned_tools: Vec<ToolPin>,
}

/// Installs the host policy and cargo shim next to an already installed binary.
pub(crate) fn install_tools(
    home: &Path,
    current_exe: &Path,
    search_path: Option<&OsStr>,
    owner: &str,
    transfer_from: Option<&str>,
    drain_timeout: Option<Duration>,
) -> Result<()> {
    if crate::build_info::DIRTY != "false" {
        return Err(Error::new(
            ErrorCode::Configuration,
            "install-tools refuses a dirty or unverifiable source tree; commit the build inputs and rebuild from clean source",
        ));
    }
    let textfile_directory = std::cell::RefCell::new(None);
    install_tools_owned(
        home,
        current_exe,
        search_path,
        &crate::writable::shared_root()?,
        owner,
        transfer_from,
        crate::build_info::COMMIT,
        drain_timeout,
        Config::measured_default,
        || {
            install_npm_packages(&read_manifest(TOOL_MANIFEST)?)?;
            textfile_directory.replace(Some(detect_textfile_directory()?));
            Ok(())
        },
        || {
            let shared = crate::writable::shared_root()?;
            retire_legacy_monitoring(home)?;
            install_monitoring(
                home,
                &home.join(".local/bin/ix-build"),
                &shared,
                &shared.join("policy.json"),
                textfile_directory.borrow().as_deref().ok_or_else(|| {
                    Error::new(ErrorCode::State, "monitoring preflight result missing")
                })?,
            )?;
            migrate_metric_file(textfile_directory.borrow().as_deref().ok_or_else(|| {
                Error::new(ErrorCode::State, "monitoring preflight result missing")
            })?)?;
            activate_monitoring(home)
        },
        |election, token| {
            let shared = crate::writable::shared_root()?;
            crate::scheduler::handoff(
                &crate::store::Store {
                    path: shared.clone(),
                    config_path: shared.join("policy.json"),
                },
                &home.join(".local/bin/ix-build"),
                election,
                token,
            )
            .map(Some)
        },
    )
}

#[cfg(test)]
fn install_tools_with_measurement(
    home: &Path,
    current_exe: &Path,
    search_path: Option<&OsStr>,
    shared: &Path,
    measure_policy: impl FnOnce() -> Result<Config>,
) -> Result<()> {
    install_tools_owned(
        home,
        current_exe,
        search_path,
        shared,
        "fixture-rollout",
        None,
        "1111111111111111111111111111111111111111",
        None,
        measure_policy,
        || Ok(()),
        || Ok(()),
        |_, _| Ok(None),
    )
}

#[allow(clippy::too_many_arguments)]
fn install_tools_owned(
    home: &Path,
    current_exe: &Path,
    search_path: Option<&OsStr>,
    shared: &Path,
    owner: &str,
    transfer_from: Option<&str>,
    source: &str,
    drain_timeout: Option<Duration>,
    measure_policy: impl FnOnce() -> Result<Config>,
    prepare: impl FnOnce() -> Result<()>,
    finish: impl FnOnce() -> Result<()>,
    resume: impl FnOnce(std::fs::File, &str) -> Result<Option<std::process::Child>>,
) -> Result<()> {
    crate::writable::migrate_namespace(&home.join(".ix/ix-build"), &home.join(".ix/locked-build"))?;
    let legacy_shared = crate::writable::legacy_shared_path();
    if shared
        == legacy_shared
            .canonicalize()
            .unwrap_or_else(|_| legacy_shared.clone())
    {
        crate::writable::migrate_namespace(
            &legacy_shared.with_file_name(format!("ix-build-{}", nix::unistd::geteuid())),
            &legacy_shared,
        )?;
    }
    let provenance = crate::provenance::InstallGuard::acquire(
        home,
        owner,
        transfer_from,
        source,
        current_exe,
        crate::runtime::now_ms()?,
    )?;
    let manifest = read_manifest(TOOL_MANIFEST)?;
    let installed_path = home.join(".local/bin/ix-build");
    let installed_bin_dir = installed_path.parent().ok_or_else(|| {
        Error::new(
            ErrorCode::Configuration,
            "installed binary path has no parent",
        )
    })?;
    std::fs::create_dir_all(installed_bin_dir)?;
    let config_dir = crate::writable::host_root(Some(home));
    let shared = crate::writable::private_directory(shared)?;
    let checked = transaction::Checked::new(home, &shared)?;
    let preflight = || -> Result<Option<Config>> {
        prepare()?;
        if !shared.join("policy.json").exists()
            && !config_dir.join("policy.json").exists()
            && !home.join(".config/ix-build/policy.json").exists()
            && !home.join(".config/locked-build/policy.json").exists()
        {
            Ok(Some(measure_policy()?))
        } else {
            Ok(None)
        }
    };
    // Ordinary installs do all slow preparation before observing idle capacity.
    // Drain holds admission first, but leaves the state lock free throughout
    // preflight so running leases can finish. Neither path performs network
    // work between the idle check and publishing the installed replacement.
    let (held, measured_policy) = if drain_timeout.is_some() {
        let held = checked.hold(drain_timeout)?;
        let measured = preflight()?;
        (held, measured)
    } else {
        let measured = preflight()?;
        (checked.hold(drain_timeout)?, measured)
    };
    let idle = held.wait_idle()?;
    if shared.join("policy-ledger.json").exists() {
        crate::policy::check(&shared, &shared.join("policy.json"))?;
    }
    let swapped = idle.swap(current_exe, &installed_path)?;
    // Temporary compatibility command; removed once all lanes confirm migration.
    install_symlink(&installed_path, &home.join(".local/bin/locked-build"))?;
    crate::store::Store {
        path: shared.clone(),
        config_path: shared.join("policy.json"),
    }
    .publish_service_binary(&installed_path)?;
    crate::writable::install_host_namespace(&shared)?;
    let data_dir = home.join(".local/share/dev-tools");
    let shim_dir = home.join(".local/lib/ix-build/shims");
    create_private_dir(&config_dir)?;
    create_private_dir(&data_dir)?;
    create_private_dir(&shim_dir)?;

    let host_policy = config_dir.join("policy.json");
    for path in [
        home.join(".config/locked-build/policy.json"),
        home.join(".config/ix-build/policy.json"),
        host_policy.clone(),
        shared.join("policy.json"),
    ] {
        migrate_memory_fields(&path)?;
    }
    migrate_policy(&home.join(".config/locked-build/policy.json"), &host_policy)?;
    migrate_policy(&home.join(".config/ix-build/policy.json"), &host_policy)?;
    let policy_path = shared.join("policy.json");
    let adopt_host = !policy_path.exists()
        || (host_policy.exists()
            && host_policy.canonicalize().ok() != policy_path.canonicalize().ok());
    let mut policy = if adopt_host && host_policy.exists() {
        Config::read(&host_policy)?
    } else if policy_path.exists() {
        Config::read(&policy_path)?
    } else {
        let mut policy = measured_policy.ok_or_else(|| {
            Error::new(
                ErrorCode::Configuration,
                "host policy disappeared after preflight; retry installation",
            )
        })?;
        policy.host = policy.host.to_ascii_lowercase();
        policy.real_cargo = discover_executable(search_path, "cargo", current_exe);
        if policy.target_root.is_none() {
            policy.target_root = Some(config_dir.join("targets"));
        }
        policy
    };
    policy
        .priority_classes
        .entry("kani".to_owned())
        .or_insert(0);
    policy
        .priority_classes
        .entry("arch-test".to_owned())
        .or_insert(2);
    let bytes = serde_json::to_vec_pretty(&policy)?;
    if adopt_host && policy_path.exists() && std::fs::read(&policy_path)? != bytes {
        crate::writable::publish_file(
            &shared.join("policy.before-host-migration.json"),
            &std::fs::read(&policy_path)?,
            false,
        )?;
    }
    write_if_changed(&policy_path, &bytes, 0o600)?;
    if std::fs::read_link(&host_policy).ok().as_deref() != Some(policy_path.as_path()) {
        let next = config_dir.join(format!(".policy-alias-{}.next", crate::store::random_id()?));
        symlink(&policy_path, &next)?;
        std::fs::rename(next, &host_policy)?;
    }
    crate::writable::shared_state(&config_dir.join("state"), &shared)?;

    let shim_path = shim_dir.join("cargo");
    install_symlink(&installed_path, &shim_path)?;
    let installed_record = InstalledRecord {
        schema_version: MANIFEST_VERSION,
        ix_build_version: env!("CARGO_PKG_VERSION").to_owned(),
        policy_path: policy_path.clone(),
        cargo_shim: shim_path.clone(),
        pinned_tools: manifest.tools,
    };
    let record_path = data_dir.join("installed.json");
    write_if_changed(
        &record_path,
        &serde_json::to_vec_pretty(&installed_record)?,
        0o600,
    )?;

    let path_line = format!(
        "export PATH=\"{}:$HOME/.local/bin:$PATH\"",
        shell_quote_path(&shim_dir)
    );
    for profile in [".profile", ".bashrc", ".zshrc"] {
        ensure_profile_line(&home.join(profile), &path_line)?;
    }
    let verified = swapped.verify(|| {
        if crate::provenance::artifact_hash(&installed_path)? != provenance.record.artifact_sha256 {
            return Err(Error::new(
                ErrorCode::Configuration,
                "installed binary changed during installation; provenance not published",
            ));
        }
        finish()?;
        crate::policy::baseline(&shared, &shared.join("policy.json"), "install")?;
        provenance.publish()
    })?;
    verified.resume(resume)?;

    println!("installed ix-build {}", env!("CARGO_PKG_VERSION"));
    println!("policy: {}", policy_path.display());
    println!("cargo shim: {}", shim_path.display());
    println!("binary: {}", installed_path.display());
    println!("version record: {}", record_path.display());
    Ok(())
}

/// Admission is held independently of state writes, so workers can finish and
/// callers can enqueue while installation waits. Dropping either file releases
/// its OS lock, including on error or process exit. Never unlink lock files.
/// Provenance: SWM-429. Trace: FR-011-AC-7, FR-005-AC-4
struct InstallHold {
    admission: Option<std::fs::File>,
    state: Option<std::fs::File>,
    election: Option<std::fs::File>,
    durable: Option<(PathBuf, String)>,
    started: Instant,
}

impl InstallHold {
    fn open(&mut self) -> Result<()> {
        if let Some((path, id)) = self.durable.as_ref() {
            crate::queue::resume_locked(
                &crate::store::Store {
                    path: path.clone(),
                    config_path: path.join("policy.json"),
                },
                Some(id),
            )?;
        }
        self.durable = None;
        Ok(())
    }
}

impl Drop for InstallHold {
    fn drop(&mut self) {
        drop(self.state.take());
        if let Some((path, id)) = self.durable.take() {
            let store = crate::store::Store {
                config_path: path.join("policy.json"),
                path,
            };
            if let Err(error) = crate::queue::resume_locked(&store, Some(&id)) {
                eprintln!(
                    "install drain resume failed: {error}; use queue resume after repairing state"
                );
            }
        }
        drop(self.admission.take());
    }
}

fn begin_hold(shared: &Path, drain_timeout: Option<Duration>) -> Result<InstallHold> {
    let store = crate::store::Store {
        path: shared.to_path_buf(),
        config_path: shared.join("policy.json"),
    };
    let started = Instant::now();
    let hold = {
        let timeout = drain_timeout.unwrap_or(Duration::from_secs(5));
        loop {
            if let Some(hold) = store.try_lock_member("admission.lock")? {
                break Some(hold);
            }
            if started.elapsed() >= timeout {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "installation drain timed out acquiring admission hold; nothing cancelled",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    // Construct cleanup ownership immediately after persistence. Any later
    // returned error resumes the exact hold created by this installation.
    let durable = {
        let durable = crate::queue::pause_locked(&store, "install_drain", 900_000)?;
        Some((shared.to_path_buf(), durable.id))
    };
    let guard = InstallHold {
        admission: hold,
        state: None,
        election: None,
        durable,
        started,
    };
    Ok(guard)
}

#[cfg(test)]
fn install_admission_hold(shared: &Path, drain_timeout: Option<Duration>) -> Result<InstallHold> {
    wait_idle(begin_hold(shared, drain_timeout)?, shared, drain_timeout)
}

fn wait_idle(
    mut guard: InstallHold,
    shared: &Path,
    drain_timeout: Option<Duration>,
) -> Result<InstallHold> {
    let store = crate::store::Store {
        path: shared.to_path_buf(),
        config_path: shared.join("policy.json"),
    };
    let started = guard.started;
    guard.election = {
        let timeout = drain_timeout.unwrap_or(Duration::from_secs(5));
        loop {
            if let Some(election) = store.try_lock_member("scheduler.lock")? {
                break Some(election);
            }
            let protocol = std::fs::read(shared.join("scheduler.lock"))?;
            if drain_timeout.is_some()
                && protocol.get(12..) != Some(crate::scheduler::DRAIN_PROTOCOL)
            {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "active scheduler does not support install drain; bootstrap with an empty-queue installation",
                ));
            }
            if started.elapsed() >= timeout {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "installation drain timed out waiting for scheduler handoff; admission hold released",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    loop {
        if let Some(state_lock) = store.try_lock_member("lock")? {
            let busy = match std::fs::read(shared.join("state.json")) {
                Ok(bytes) => {
                    let state: crate::model::State = serde_json::from_slice(&bytes)?;
                    let mut busy = false;
                    for request in &state.requests {
                        if request.phase.terminal() {
                            continue;
                        }
                        if drain_timeout.is_none() || request.phase != crate::model::Phase::Queued {
                            busy = true;
                            continue;
                        }
                        if store
                            .try_lock_member(&format!("{}.spawn.lock", request.id.0))?
                            .is_none()
                        {
                            let supported = store.claim(&request.id)?.is_some_and(|claim| {
                                claim.protocol == 2
                                    && claim.store_id == state.store_id
                                    && claim.request_id == request.id
                                    && claim.generation == request.worker_generation
                            });
                            if !supported {
                                return Err(Error::new(
                                    ErrorCode::Configuration,
                                    "queued supervisor does not support install drain; bootstrap with an empty-queue installation",
                                ));
                            }
                            busy |= store
                                .claim(&request.id)?
                                .is_some_and(|claim| claim.worker.is_some());
                        }
                    }
                    busy
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            };
            if !busy {
                guard.state = Some(state_lock);
                return Ok(guard);
            }
            if drain_timeout.is_none() {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "drain queued and running leases before install-tools, or use --drain; run installation outside an admitted lease",
                ));
            }
            // Running supervisors need the state lock to checkpoint completion.
            drop(state_lock);
        }
        if drain_timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
            return Err(Error::new(
                ErrorCode::Configuration,
                "installation drain timed out; nothing cancelled; admission hold released",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn migrate_policy(old: &Path, new: &Path) -> Result<()> {
    if !old.try_exists()? {
        return Ok(());
    }
    // Validate before moving; never measure over hand-tuned host settings.
    Config::read(old)?;
    if new.try_exists()? {
        if std::fs::read(old)? != std::fs::read(new)? {
            return Err(Error::new(
                ErrorCode::Configuration,
                "both old and new policies exist and differ; preserving both",
            ));
        }
    } else {
        // Exclusive creation refuses to overwrite a concurrently installed policy.
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(new)?;
        file.write_all(&std::fs::read(old)?)?;
        file.sync_all()?;
    }
    std::fs::remove_file(old)?;
    Ok(())
}

fn migrate_memory_fields(path: &Path) -> Result<()> {
    if !path.try_exists()? {
        return Ok(());
    }
    let path = path.canonicalize()?;
    let before = std::fs::read(&path)?;
    let mut policy: serde_json::Value = serde_json::from_slice(&before)?;
    let fields = policy
        .as_object_mut()
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "host policy must be an object"))?;
    if fields.remove("memory_per_job_bytes").is_none() {
        return Ok(());
    }
    fields
        .entry("memory_reserve_per_job_bytes")
        .or_insert(3_000_000_000_u64.into());
    fields
        .entry("memory_limit_per_job_bytes")
        .or_insert(8_000_000_000_u64.into());
    // Preserve a deliberately smaller per-class cap without reserving more
    // than it can enforce after the split.
    let reserve = fields
        .get("memory_reserve_per_job_bytes")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(3_000_000_000);
    let overrides: Vec<_> = fields
        .get("memory_per_job_by_class")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(class, limit)| {
            limit
                .as_u64()
                .filter(|limit| *limit < reserve)
                .map(|limit| (class.clone(), limit))
        })
        .collect();
    if !overrides.is_empty() {
        let reservations = fields
            .entry("memory_reserve_per_job_by_class")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(reservations) = reservations.as_object_mut() {
            for (class, bytes) in overrides {
                reservations.entry(class).or_insert(bytes.into());
            }
        }
    }
    let validated: Config = serde_json::from_value(policy.clone())?;
    validated.validate()?;
    let backup = path.with_file_name("policy.before-memory-split.json");
    if !crate::writable::publish_file(&backup, &before, false)? && std::fs::read(&backup)? != before
    {
        return Err(Error::new(
            ErrorCode::Configuration,
            "memory-policy migration backup conflicts; preserving policy",
        ));
    }
    crate::writable::publish_file(&path, &serde_json::to_vec_pretty(&policy)?, true)?;
    Ok(())
}

fn install_npm_packages(manifest: &ToolManifest) -> Result<()> {
    let arguments = npm_install_arguments(&manifest.tools)?;
    if arguments.is_empty() {
        return Ok(());
    }
    let status = Command::new("npm").args(arguments).status()?;
    if !status.success() {
        return Err(Error::new(
            ErrorCode::Configuration,
            format!("npm install failed with status {status}"),
        ));
    }
    Ok(())
}

fn npm_install_arguments(tools: &[ToolPin]) -> Result<Vec<String>> {
    let mut packages = Vec::new();
    for tool in tools {
        let Some(package) = tool.npm_package.as_deref() else {
            continue;
        };
        let version = tool.version.as_deref().ok_or_else(|| {
            Error::new(
                ErrorCode::Configuration,
                format!("npm package for {} has no pinned version", tool.name),
            )
        })?;
        packages.push(format!("{package}@{version}"));
    }
    if packages.is_empty() {
        return Ok(Vec::new());
    }
    let mut arguments = vec![
        "install".to_owned(),
        "--global".to_owned(),
        format!("--registry={NPM_REGISTRY}"),
        NPM_SCOPE_REGISTRY.to_owned(),
    ];
    arguments.extend(packages);
    Ok(arguments)
}

/// Checks executable presence and minimum versions from the manifest.
pub(crate) fn check_install(manifest_path: Option<&Path>) -> Result<i32> {
    let manifest = match manifest_path {
        Some(path) => read_manifest(&std::fs::read_to_string(path)?)?,
        None => read_manifest(TOOL_MANIFEST)?,
    };
    let mut mismatches = 0_u32;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = &home {
        let root = crate::writable::host_root(Some(home));
        if root.join("state").exists() || root.join("policy.json").exists() {
            let shared = root.join("state").canonicalize()?;
            crate::policy::check(&shared, &root.join("policy.json"))?;
        }
        match crate::provenance::check(home) {
            Ok((matches, line)) => {
                println!("{line}");
                mismatches += u32::from(!matches);
            }
            Err(error) => {
                println!("mismatch ix-build provenance {}", error.message);
                mismatches += 1;
            }
        }
    } else {
        println!("mismatch ix-build provenance HOME is not set");
        mismatches += 1;
    }
    let search_path = std::env::var_os("PATH");
    for pin in &manifest.tools {
        let expected = pin.version.as_deref().unwrap_or("present");
        match check_tool(pin, search_path.as_deref(), home.as_deref()) {
            Ok(observed) => println!("ok {} {observed}", pin.name),
            Err(detail) => {
                println!("mismatch {} expected={expected} {detail}", pin.name);
                mismatches = mismatches.saturating_add(1);
            }
        }
    }
    Ok(i32::from(mismatches != 0))
}

fn read_manifest(contents: &str) -> Result<ToolManifest> {
    let manifest: ToolManifest = serde_json::from_str(contents)
        .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
    if manifest.schema_version != MANIFEST_VERSION
        || manifest.tools.is_empty()
        || manifest.tools.iter().any(|tool| {
            tool.name.is_empty()
                || tool.executable.is_empty()
                || tool
                    .version
                    .as_ref()
                    .is_some_and(|version| semver::Version::parse(version).is_err())
        })
    {
        return Err(Error::new(
            ErrorCode::Configuration,
            "tool manifest has an unsupported version or invalid pins",
        ));
    }
    Ok(manifest)
}

fn check_tool(
    pin: &ToolPin,
    search_path: Option<&OsStr>,
    home: Option<&Path>,
) -> std::result::Result<String, String> {
    let executable = resolve_tool_executable(
        &pin.executable,
        search_path,
        if pin.version.is_some() { home } else { None },
    )
    .ok_or_else(|| "unavailable=not-found".to_owned())?;
    let Some(version) = pin.version.as_deref() else {
        return Ok("present".to_owned());
    };
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .map_err(|error| format!("unavailable={error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        return Err(format!("exit={} detail={detail}", output.status));
    }
    let observed = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if version_matches(&pin.name, version, &observed) {
        reported_version(&pin.name, &observed)
            .map(str::to_owned)
            .ok_or_else(|| format!("observed={observed}"))
    } else {
        Err(format!("observed={observed}"))
    }
}

fn version_matches(name: &str, expected: &str, observed: &str) -> bool {
    let Some(candidate) = reported_version(name, observed) else {
        return false;
    };
    match (
        semver::Version::parse(expected),
        semver::Version::parse(candidate),
    ) {
        (Ok(minimum), Ok(installed)) => !installed.cmp_precedence(&minimum).is_lt(),
        _ => false,
    }
}

fn reported_version<'a>(name: &str, observed: &'a str) -> Option<&'a str> {
    observed
        .strip_prefix(name)
        .map(str::trim_start)
        .unwrap_or(observed)
        .split_whitespace()
        .next()
}

fn discover_executable(
    search_path: Option<&OsStr>,
    name: &str,
    current_exe: &Path,
) -> Option<PathBuf> {
    let search_path = search_path?;
    std::env::split_paths(search_path)
        .map(|directory| directory.join(name))
        .find(|candidate| {
            candidate.is_file()
                && candidate
                    .canonicalize()
                    .ok()
                    .zip(current_exe.canonicalize().ok())
                    .is_some_and(|(candidate, current)| {
                        candidate != current
                            && candidate.file_name() != Some(OsStr::new("ix-build"))
                    })
        })
}

fn resolve_tool_executable(
    name: &str,
    search_path: Option<&OsStr>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let mut candidates = search_path
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join(name))
        .collect::<Vec<_>>();
    if let Some(home) = home {
        candidates.push(home.join(".local/bin").join(name));
        candidates.push(home.join(".cargo/bin").join(name));
    }
    candidates.into_iter().find_map(|candidate| {
        if !candidate.is_file()
            || nix::unistd::access(&candidate, nix::unistd::AccessFlags::X_OK).is_err()
        {
            return None;
        }
        let resolved = candidate.canonicalize().ok()?;
        if name == "cargo" && resolved.file_name() == Some(OsStr::new("ix-build")) {
            return None;
        }
        Some(candidate)
    })
}

fn copy_file_if_changed(source: &Path, destination: &Path) -> Result<()> {
    let source_contents = std::fs::read(source)?;
    if std::fs::read(destination).ok().as_deref() == Some(source_contents.as_slice()) {
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755))?;
        return Ok(());
    }
    let temporary = destination.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(0o755);
    let mut output = options.open(&temporary)?;
    output.write_all(&source_contents)?;
    output.sync_all()?;
    std::fs::rename(temporary, destination)?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_if_changed(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    if std::fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "installation path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(mode);
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn install_symlink(target: &Path, link: &Path) -> Result<()> {
    let target = target.canonicalize()?;
    if std::fs::read_link(link).ok().as_deref() == Some(target.as_path()) {
        return Ok(());
    }
    let temporary = link.with_extension("tmp");
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    symlink(target, &temporary)?;
    std::fs::rename(temporary, link)?;
    Ok(())
}

fn ensure_profile_line(path: &Path, line: &str) -> Result<()> {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    let current = current.replace(".local/lib/locked-build/shims", ".local/lib/ix-build/shims");
    if current.lines().any(|existing| existing == line) {
        return write_if_changed(path, current.as_bytes(), 0o600);
    }
    let mut updated = current;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(line);
    updated.push('\n');
    write_if_changed(path, updated.as_bytes(), 0o600)
}

fn shell_quote_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('"', "\\\"")
        .replace('$', "\\$")
}

#[cfg(test)]
#[path = "install/drain_tests.rs"]
mod drain_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// Trace: FR-011-AC-7, FR-005-AC-4. Provenance: SWM-431.
    #[test]
    fn host_install_migrates_legacy_policy_profile_and_compatibility_command() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let old = crate::writable::private_directory(&root.path().join("old-host"))?;
        std::fs::create_dir_all(home.join(".ix"))?;
        // Use a real directory, as on hosts installed before the rename.
        std::fs::rename(&old, home.join(".ix/locked-build"))?;
        let old = home.join(".ix/locked-build");
        std::fs::write(old.join("policy.json"), br#"{"host":"test-host","capacity_units":4,"maximum_units":4,"cargo_build_jobs":4,"rust_test_threads":4}"#)?;
        std::fs::write(
            home.join(".zshrc"),
            format!(
                "export PATH=\"{}:$HOME/.local/bin:$PATH\"\n",
                home.join(".local/lib/locked-build/shims").display()
            ),
        )?;
        let shared = root.path().join("shared");
        let source = std::env::current_exe()?;
        install_tools_with_measurement(&home, &source, None, &shared, || {
            Err(Error::new(
                ErrorCode::State,
                "existing policy must be preserved",
            ))
        })?;
        assert_eq!(
            home.join(".ix/ix-build").canonicalize()?,
            old.canonicalize()?
        );
        assert_eq!(
            std::fs::read_link(home.join(".local/bin/locked-build"))?,
            home.join(".local/bin/ix-build").canonicalize()?
        );
        assert_eq!(
            Config::read(&shared.join("policy.json"))?.cargo_build_jobs,
            Some(4)
        );
        let profile = std::fs::read_to_string(home.join(".zshrc"))?;
        assert!(!profile.contains("locked-build"));
        assert_eq!(profile.lines().count(), 1);
        assert!(profile.contains(".local/lib/ix-build/shims"));
        Ok(())
    }

    /// Provenance: SWM-412. Trace: FR-011-AC-7
    #[test]
    fn install_preserves_the_measured_fast_disk_default() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let shared = root.path().join("shared");
        let target = root.path().join("nvme/targets");
        install_tools_with_measurement(&home, &std::env::current_exe()?, None, &shared, || {
            Ok(Config {
                target_root: Some(target.clone()),
                disk_path: root.path().join("nvme"),
                ..Config::default()
            })
        })?;
        assert_eq!(
            Config::read(&shared.join("policy.json"))?.target_root,
            Some(target)
        );
        Ok(())
    }

    /// Provenance: SWM-403. Trace: FR-011-AC-7
    #[test]
    fn install_records_the_exact_artifact_and_preserves_host_policy() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let artifact = root.path().join("artifact");
        std::fs::write(&artifact, b"abc")?;
        install_tools_with_measurement(&home, &artifact, None, &root.path().join("state"), || {
            Ok(Config::default())
        })?;
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join(".ix/ix-build/install.json"))?)?;
        assert_eq!(
            record["artifact_sha256"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(
            record["installed_at_ms"]
                .as_u64()
                .is_some_and(|time| time > 0)
        );
        assert_eq!(
            record["source_commit"],
            "1111111111111111111111111111111111111111"
        );
        assert_eq!(record["owner"], "fixture-rollout");
        Ok(())
    }

    /// Trace: FR-011-AC-7, FR-005-AC-4
    #[test]
    fn install_refuses_a_live_queue_before_changing_binary_or_policy() -> Result<()> {
        let root = tempfile::tempdir()?;
        let shared = root.path().join("shared");
        let policy = root.path().join("policy.json");
        std::fs::write(&policy, serde_json::to_vec(&Config::default())?)?;
        let store = crate::store::Store::open(&shared, &policy)?;
        crate::runtime::enqueue(&store, "fixture", "SWM-379", 1, 0, None, &["true".into()])?;
        let before = std::fs::read(shared.join("state.json"))?;
        let home = root.path().join("home");
        std::fs::create_dir_all(home.join(".local/bin"))?;
        let binary = home.join(".local/bin/ix-build");
        std::fs::write(&binary, b"old installed bytes")?;
        let result =
            install_tools_with_measurement(&home, &std::env::current_exe()?, None, &shared, || {
                Ok(Config::default())
            });
        let error = result
            .err()
            .ok_or_else(|| Error::new(ErrorCode::State, "live queue was not refused"))?;
        assert!(
            error.message.contains("drain queued and running leases"),
            "{error}"
        );
        assert_eq!(std::fs::read(binary)?, b"old installed bytes");
        assert_eq!(std::fs::read(shared.join("state.json"))?, before);
        assert!(!shared.join("policy.json").exists());
        Ok(())
    }

    /// Trace: FR-011-AC-7
    #[test]
    fn memory_policy_split_migrates_once_and_preserves_host_tuning() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("policy.json");
        let before = br#"{"host":"titan","cargo_build_jobs":4,"rust_test_threads":4,"disk_reserve_bytes":100000000000,"hold_ms":5400000,"memory_per_job_bytes":6000000000,"memory_per_job_by_class":{"point":2500000000}}"#;
        std::fs::write(&path, before)?;
        migrate_memory_fields(&path)?;
        let policy = Config::read(&path)?;
        assert_eq!(
            (
                policy.memory_reserve_per_job_bytes,
                policy.memory_limit_per_job_bytes
            ),
            (3_000_000_000, 8_000_000_000)
        );
        assert_eq!(
            policy.memory_reserve_per_job_by_class.get("point"),
            Some(&2_500_000_000)
        );
        assert_eq!(
            (
                policy.cargo_build_jobs,
                policy.rust_test_threads,
                policy.disk_reserve_bytes,
                policy.hold_ms
            ),
            (Some(4), Some(4), 100_000_000_000, 5_400_000)
        );
        assert_eq!(
            std::fs::read(root.path().join("policy.before-memory-split.json"))?,
            before
        );
        let installed = std::fs::read(&path)?;
        migrate_memory_fields(&path)?;
        assert_eq!(std::fs::read(&path)?, installed);
        let explicit = root.path().join("explicit");
        std::fs::create_dir(&explicit)?;
        let explicit = explicit.join("policy.json");
        std::fs::write(&explicit, br#"{"memory_per_job_bytes":6000000000,"memory_reserve_per_job_bytes":1000000000,"memory_limit_per_job_bytes":16000000000}"#)?;
        migrate_memory_fields(&explicit)?;
        let policy = Config::read(&explicit)?;
        assert_eq!(
            (
                policy.memory_reserve_per_job_bytes,
                policy.memory_limit_per_job_bytes
            ),
            (1_000_000_000, 16_000_000_000)
        );
        Ok(())
    }

    /// Provenance: SWM-374. Trace: FR-011-AC-7
    #[test]
    fn migration_moves_hand_tuned_policy_without_regeneration() -> Result<()> {
        let root = tempfile::tempdir()?;
        let old = root.path().join("old-policy.json");
        let new = root.path().join("policy.json");
        let bytes = br#"{"host":"titan","capacity_units":4,"maximum_units":4,"cargo_build_jobs":4,"rust_test_threads":4,"disk_reserve_bytes":100000000000,"hold_ms":5400000}"#;
        std::fs::write(&old, bytes)?;
        migrate_policy(&old, &new)?;
        assert!(!old.exists());
        assert_eq!(std::fs::read(&new)?, bytes);
        let policy = Config::read(&new)?;
        assert_eq!(policy.cargo_build_jobs, Some(4));
        assert_eq!(policy.rust_test_threads, Some(4));
        assert_eq!(policy.disk_reserve_bytes, 100_000_000_000);
        assert_eq!(policy.hold_ms, 5_400_000);
        std::fs::write(&old, b"{\"host\":\"luna\"}")?;
        assert!(migrate_policy(&old, &new).is_err());
        assert_eq!(std::fs::read(&new)?, bytes);
        assert!(old.exists());
        Ok(())
    }

    #[test]
    fn checked_in_pins_have_hand_written_version_requirements_and_executable_names() -> Result<()> {
        let manifest = read_manifest(TOOL_MANIFEST)?;
        let expected = [
            ("ix-build", "ix-build", Some("0.2.0"), None),
            (
                "quire",
                "quire",
                Some("0.53.3"),
                Some("@agent-ix/quire-cli"),
            ),
            ("quoin", "quoin", Some("0.37.0"), Some("@agent-ix/quoin")),
            ("ix-board", "ix-board", Some("0.12.2"), None),
            (
                "ix-flow",
                "ix-flow",
                Some("0.3.5"),
                Some("@agent-ix/ix-flow"),
            ),
            ("ix-trace-run", "ix-trace-run", None, None),
        ];
        let actual: Vec<_> = manifest
            .tools
            .iter()
            .map(|tool| {
                (
                    tool.name.as_str(),
                    tool.executable.as_str(),
                    tool.version.as_deref(),
                    tool.npm_package.as_deref(),
                )
            })
            .collect();
        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn npm_install_argv_uses_explicit_public_registry_and_pinned_packages() -> Result<()> {
        let tools = [
            ToolPin {
                name: "quire".to_owned(),
                executable: "quire".to_owned(),
                version: Some("0.53.3".to_owned()),
                npm_package: Some("@agent-ix/quire-cli".to_owned()),
            },
            ToolPin {
                name: "quoin".to_owned(),
                executable: "quoin".to_owned(),
                version: Some("0.37.0".to_owned()),
                npm_package: Some("@agent-ix/quoin".to_owned()),
            },
            ToolPin {
                name: "ix-flow".to_owned(),
                executable: "ix-flow".to_owned(),
                version: Some("0.3.5".to_owned()),
                npm_package: Some("@agent-ix/ix-flow".to_owned()),
            },
        ];

        assert_eq!(
            npm_install_arguments(&tools)?,
            vec![
                "install",
                "--global",
                "--registry=https://registry.npmjs.org/",
                "--@agent-ix:registry=https://registry.npmjs.org/",
                "@agent-ix/quire-cli@0.53.3",
                "@agent-ix/quoin@0.37.0",
                "@agent-ix/ix-flow@0.3.5",
            ]
        );
        Ok(())
    }

    #[test]
    fn presence_only_manifest_pin_passes_even_when_help_fails() -> Result<()> {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir()?;
        let executable = root.path().join("presence-tool");
        symlink("/usr/bin/false", &executable)?;
        assert_eq!(
            Command::new(&executable).arg("--help").status()?.code(),
            Some(1)
        );
        let manifest = read_manifest(
            r#"{"schema_version":1,"tools":[{"name":"presence-tool","executable":"presence-tool"}]}"#,
        )?;
        let search_path = std::env::join_paths([root.path()])
            .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
        let pin = manifest.tools.first().ok_or_else(|| {
            Error::new(
                ErrorCode::Configuration,
                "presence-only fixture pin missing",
            )
        })?;

        assert_eq!(pin.version, None);
        assert_eq!(
            check_tool(pin, Some(&search_path), None),
            Ok("present".to_owned())
        );
        Ok(())
    }

    #[test]
    fn presence_only_manifest_pin_mismatches_missing_executable() -> Result<()> {
        let root = tempfile::tempdir()?;
        let search_path = std::env::join_paths([root.path()])
            .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
        let missing = ToolPin {
            name: "presence-tool".to_owned(),
            executable: "missing-tool".to_owned(),
            version: None,
            npm_package: None,
        };
        assert_eq!(
            check_tool(&missing, Some(&search_path), None),
            Err("unavailable=not-found".to_owned())
        );
        Ok(())
    }

    /// Trace: FR-011-AC-7
    #[test]
    fn presence_only_requires_path_and_actual_execute_permission() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home_bin = root.path().join("home/.local/bin");
        let path_bin = root.path().join("path");
        std::fs::create_dir_all(&home_bin)?;
        std::fs::create_dir(&path_bin)?;
        let executable = home_bin.join("presence-tool");
        std::fs::write(&executable, b"not invoked")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let pin = ToolPin {
            name: "presence-tool".into(),
            executable: "presence-tool".into(),
            version: None,
            npm_package: None,
        };
        let path = std::env::join_paths([&path_bin])
            .map_err(|error| Error::new(ErrorCode::State, error.to_string()))?;
        assert_eq!(
            check_tool(&pin, Some(&path), Some(&root.path().join("home"))),
            Err("unavailable=not-found".into())
        );
        let in_path = path_bin.join("presence-tool");
        std::fs::rename(&executable, &in_path)?;
        assert_eq!(check_tool(&pin, Some(&path), None), Ok("present".into()));
        std::fs::set_permissions(&in_path, std::fs::Permissions::from_mode(0o001))?;
        if !nix::unistd::geteuid().is_root() {
            assert_eq!(
                check_tool(&pin, Some(&path), None),
                Err("unavailable=not-found".into())
            );
        }
        Ok(())
    }

    #[test]
    fn version_check_recognizes_cli_version_formats() {
        assert!(version_matches(
            "quire",
            "0.53.3",
            "quire 0.53.3 (engine 0.63.2)"
        ));
        assert!(version_matches("quoin", "0.37.0", "0.37.0"));
        assert!(!version_matches("quoin", "0.37.0", "quoin 0.37.0-rc.1"));
        assert!(version_matches("quoin", "0.37.0", "quoin 0.37.0+local"));
        assert!(version_matches(
            "quoin",
            "0.37.0+local",
            "quoin 0.37.0+local"
        ));
        assert!(!version_matches("quoin", "0.37.0", "0.36.9"));
        assert!(version_matches("ix-board", "0.12.2", "ix-board 0.12.3"));
        assert!(!version_matches("ix-board", "0.12.2", "ix-board 0.12.1"));
        assert!(version_matches("ix-board", "0.12.2", "ix-board 0.13.0"));
        assert!(version_matches("ix-board", "0.12.2", "ix-board 1.0.0"));
        assert!(version_matches("ix-board", "0.12.2", "ix-board 0.12.10"));
        assert!(!version_matches("ix-board", "0.12.2", "ix-board unknown"));
        assert!(!version_matches(
            "ix-board",
            "0.12.2",
            "ix-board 0.12.2-rc.1"
        ));
    }

    #[test]
    fn version_check_skips_the_shim_and_finds_real_cargo_after_it() -> Result<()> {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir()?;
        let shim_dir = root.path().join("shim");
        let cargo_dir = root.path().join("cargo");
        std::fs::create_dir_all(&shim_dir)?;
        std::fs::create_dir_all(&cargo_dir)?;
        let shim_target = root.path().join("ix-build");
        let rustup = cargo_dir.join("rustup");
        let real_cargo = cargo_dir.join("cargo");
        std::fs::write(&shim_target, b"shim")?;
        std::fs::write(&rustup, b"rustup")?;
        std::fs::set_permissions(&shim_target, std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755))?;
        symlink(&shim_target, shim_dir.join("cargo"))?;
        symlink(&rustup, &real_cargo)?;
        let search_path = std::env::join_paths([&shim_dir, &cargo_dir])
            .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;

        let resolved = resolve_tool_executable("cargo", Some(&search_path), None);

        assert_eq!(resolved, Some(real_cargo));
        Ok(())
    }

    #[test]
    fn install_is_idempotent_for_managed_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let file = root.path().join("nested/record.json");
        write_if_changed(&file, b"{\"version\":1}\n", 0o600)?;
        let first = std::fs::metadata(&file)?.modified()?;
        write_if_changed(&file, b"{\"version\":1}\n", 0o600)?;
        assert_eq!(std::fs::metadata(&file)?.modified()?, first);
        Ok(())
    }

    #[test]
    fn installed_binary_copy_is_a_no_op_when_contents_match() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let destination = root.path().join("bin/ix-build");
        std::fs::write(&source, b"binary bytes")?;
        std::fs::create_dir_all(destination.parent().ok_or_else(|| {
            Error::new(ErrorCode::Configuration, "binary destination has no parent")
        })?)?;
        copy_file_if_changed(&source, &destination)?;
        let first = std::fs::metadata(&destination)?.modified()?;
        copy_file_if_changed(&source, &destination)?;
        assert_eq!(std::fs::metadata(&destination)?.modified()?, first);
        assert_eq!(std::fs::read(destination)?, b"binary bytes");
        Ok(())
    }

    #[test]
    fn installed_binary_copy_repairs_executable_mode_without_rewriting_contents() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let destination = root.path().join("bin/ix-build");
        std::fs::write(&source, b"binary bytes")?;
        std::fs::create_dir_all(destination.parent().ok_or_else(|| {
            Error::new(ErrorCode::Configuration, "binary destination has no parent")
        })?)?;
        std::fs::write(&destination, b"binary bytes")?;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o644))?;
        let first_modified = std::fs::metadata(&destination)?.modified()?;

        copy_file_if_changed(&source, &destination)?;

        let metadata = std::fs::metadata(&destination)?;
        assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
        assert_eq!(metadata.modified()?, first_modified);
        Ok(())
    }

    #[test]
    fn host_install_creates_the_policy_shim_and_record_idempotently() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        std::fs::create_dir_all(&home)?;
        let legacy = home.join(".config/ix-build/policy.json");
        std::fs::create_dir_all(
            legacy
                .parent()
                .ok_or_else(|| Error::new(ErrorCode::State, "fixture parent"))?,
        )?;
        std::fs::write(&legacy, br#"{"host":"test-host","capacity_units":4,"maximum_units":4,"cargo_build_jobs":4,"rust_test_threads":4,"disk_reserve_bytes":100000000000,"hold_ms":5400000}"#)?;
        let current_exe = std::env::current_exe()?;

        let shared = root.path().join("shared");
        let prior = crate::store::Store::open(&shared, &legacy)?;
        prior.transaction(|_, _| Ok(()))?;
        let competing = br#"{"host":"test-host","capacity_units":1,"maximum_units":1,"cargo_build_jobs":8,"rust_test_threads":8}"#;
        std::fs::write(shared.join("policy.json"), competing)?;
        // A new binary may already have opened status before installation,
        // rewriting the old config_path. The tuned host policy must still win.
        crate::store::Store::open(&shared, &shared.join("policy.json"))?
            .transaction(|_, _| Ok(()))?;
        install_tools_with_measurement(&home, &current_exe, None, &shared, || {
            let mut config: Config =
                serde_json::from_str(include_str!("../policy.example.json"))
                    .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
            config.host = "test-host".to_owned();
            config.capacity_units = 4;
            config.maximum_units = 4;
            config.validate()?;
            Ok(config)
        })?;

        let binary = home.join(".local/bin/ix-build");
        let policy_path = home.join(".ix/ix-build/policy.json");
        let shim = home.join(".local/lib/ix-build/shims/cargo");
        let record = home.join(".local/share/dev-tools/installed.json");
        assert_eq!(std::fs::read(&binary)?, std::fs::read(&current_exe)?);
        assert_eq!(
            std::fs::metadata(&binary)?.permissions().mode() & 0o111,
            0o111
        );
        assert_eq!(std::fs::read_link(&shim)?, binary.canonicalize()?);
        let policy = Config::read(&policy_path)?;
        assert_eq!(
            std::fs::read(shared.join("policy.before-host-migration.json"))?,
            competing
        );
        assert!(!legacy.exists());
        assert_eq!(policy.cargo_build_jobs, Some(4));
        assert_eq!(policy.rust_test_threads, Some(4));
        assert_eq!(policy.disk_reserve_bytes, 100_000_000_000);
        assert_eq!(policy.hold_ms, 5_400_000);
        assert_eq!(policy.priority_classes.get("arch-test"), Some(&2));
        assert_eq!(policy.priority_classes.get("kani"), Some(&0));
        let profile = std::fs::read_to_string(home.join(".zshrc"))?;
        assert!(profile.contains(".local/lib/ix-build/shims"));
        let saved_record: InstalledRecord = serde_json::from_slice(&std::fs::read(&record)?)?;
        assert_eq!(saved_record.ix_build_version, env!("CARGO_PKG_VERSION"));

        let first_modified = [
            std::fs::metadata(&binary)?.modified()?,
            std::fs::metadata(&policy_path)?.modified()?,
            std::fs::metadata(&record)?.modified()?,
            std::fs::metadata(home.join(".zshrc"))?.modified()?,
        ];
        install_tools_with_measurement(&home, &current_exe, None, &shared, || {
            Err(Error::new(
                ErrorCode::State,
                "existing policy should prevent another measurement",
            ))
        })?;
        let second_modified = [
            std::fs::metadata(&binary)?.modified()?,
            std::fs::metadata(&policy_path)?.modified()?,
            std::fs::metadata(&record)?.modified()?,
            std::fs::metadata(home.join(".zshrc"))?.modified()?,
        ];
        assert_eq!(first_modified, second_modified);
        Ok(())
    }

    #[test]
    fn host_capacity_examples_keep_the_measured_host_values() -> Result<()> {
        #[derive(Deserialize)]
        struct CapacityExamples {
            schema_version: u32,
            hosts: Vec<HostCapacity>,
        }
        #[derive(Deserialize)]
        struct HostCapacity {
            host: String,
            capacity_units: u32,
            total_memory_bytes: u64,
        }

        let examples: CapacityExamples =
            serde_json::from_str(include_str!("../host-capacity-examples.json"))?;
        let actual: Vec<_> = examples
            .hosts
            .iter()
            .map(|host| {
                (
                    host.host.as_str(),
                    host.capacity_units,
                    host.total_memory_bytes,
                )
            })
            .collect();
        assert_eq!(examples.schema_version, 1);
        assert_eq!(
            actual,
            [
                ("draco", 18, 68_719_476_736),
                ("luna", 8, 67_383_029_760),
                ("titan", 32, 65_865_936_896),
                ("terra", 14, 101_250_785_280),
            ]
        );
        Ok(())
    }
}

fn textfile_from_arguments(arguments: &[String]) -> Option<PathBuf> {
    if !arguments
        .first()
        .and_then(|s| Path::new(s).file_name())
        .is_some_and(|s| s == "node_exporter")
    {
        return None;
    }
    arguments.iter().enumerate().find_map(|(i, arg)| {
        let value = arg
            .strip_prefix("--collector.textfile.directory=")
            .map(str::to_owned)
            .or_else(|| {
                (arg == "--collector.textfile.directory")
                    .then(|| arguments.get(i + 1).cloned())
                    .flatten()
            })?;
        let path = PathBuf::from(value);
        path.is_absolute().then_some(path)
    })
}
fn detect_textfile_directory() -> Result<PathBuf> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::everything().without_tasks()),
    );
    let directories: std::collections::BTreeSet<_> = system
        .processes()
        .values()
        .filter_map(|p| {
            textfile_from_arguments(
                &p.cmd()
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    if directories.len() != 1 {
        return Err(Error::new(
            ErrorCode::Configuration,
            "expected exactly one node_exporter --collector.textfile.directory; start node_exporter with an absolute textfile directory",
        ));
    }
    directories.into_iter().next().ok_or_else(|| {
        Error::new(
            ErrorCode::Configuration,
            "node_exporter textfile directory unavailable",
        )
    })
}
fn install_monitoring(
    home: &Path,
    binary: &Path,
    state: &Path,
    config: &Path,
    textfile: &Path,
) -> Result<()> {
    let units = [
        (
            "ix-build-metrics.service",
            include_str!("../monitoring/ix-build-metrics.service"),
        ),
        (
            "ix-build-metrics.timer",
            include_str!("../monitoring/ix-build-metrics.timer"),
        ),
        (
            "ix-build-prune.service",
            include_str!("../monitoring/ix-build-prune.service"),
        ),
        (
            "ix-build-prune.timer",
            include_str!("../monitoring/ix-build-prune.timer"),
        ),
    ];
    let plists = [
        (
            "com.agent-ix.ix-build-metrics.plist",
            include_str!("../monitoring/com.agent-ix.ix-build-metrics.plist"),
        ),
        (
            "com.agent-ix.ix-build-prune.plist",
            include_str!("../monitoring/com.agent-ix.ix-build-prune.plist"),
        ),
    ];
    let (directory, files) = if cfg!(target_os = "macos") {
        (home.join("Library/LaunchAgents"), plists.as_slice())
    } else {
        (home.join(".config/systemd/user"), units.as_slice())
    };
    std::fs::create_dir_all(&directory)?;
    std::fs::create_dir_all(textfile)?;
    for (name, template) in files {
        let mut body = template.to_string();
        for (key, path) in [
            ("@BINARY@", binary),
            ("@STATE@", state),
            ("@CONFIG@", config),
            ("@OUTPUT@", &textfile.join("ix-build.prom")),
        ] {
            let value = path
                .to_str()
                .ok_or_else(|| Error::new(ErrorCode::Configuration, "non-UTF8 monitoring path"))?;
            // Quote systemd arguments; XML paths are escaped independently.
            let escaped = if cfg!(target_os = "macos") {
                value
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            } else {
                format!(
                    "\"{}\"",
                    value
                        .replace('\\', "\\\\")
                        .replace('\"', "\\\"")
                        .replace('%', "%%")
                )
            };
            body = body.replace(key, &escaped);
        }
        write_if_changed(&directory.join(name), body.as_bytes(), 0o644)?;
    }
    Ok(())
}

// Retire only the previous producer's units; queue supervisors are untouched.
fn retire_legacy_monitoring(home: &Path) -> Result<()> {
    if cfg!(target_os = "macos") {
        let domain = format!("gui/{}", nix::unistd::getuid());
        for label in [
            "com.agent-ix.locked-build-metrics",
            "com.agent-ix.locked-build-prune",
        ] {
            let target = format!("{domain}/{label}");
            if Command::new("launchctl")
                .args(["print", &target])
                .output()?
                .status
                .success()
                && !Command::new("launchctl")
                    .args(["bootout", &target])
                    .status()?
                    .success()
            {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "legacy monitoring bootout failed",
                ));
            }
            remove_legacy_file(
                &home
                    .join("Library/LaunchAgents")
                    .join(format!("{label}.plist")),
            )?;
        }
    } else {
        for kind in ["metrics", "prune"] {
            let timer = format!("locked-build-{kind}.timer");
            let service = format!("locked-build-{kind}.service");
            let directory = home.join(".config/systemd/user");
            if (directory.join(&timer).exists() || directory.join(&service).exists())
                && !Command::new("systemctl")
                    .args(["--user", "disable", "--now", &timer, &service])
                    .status()?
                    .success()
            {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "legacy monitoring disable failed",
                ));
            }
            remove_legacy_file(&directory.join(timer))?;
            remove_legacy_file(&directory.join(service))?;
        }
    }
    Ok(())
}

fn remove_legacy_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn migrate_metric_file(directory: &Path) -> Result<()> {
    let old = directory.join("locked-build.prom");
    let new = directory.join("ix-build.prom");
    if old.try_exists()? {
        if new.try_exists()? {
            remove_legacy_file(&old)?;
        } else {
            std::fs::rename(old, new)?;
        }
    }
    Ok(())
}

fn activate_monitoring(home: &Path) -> Result<()> {
    if cfg!(target_os = "macos") {
        let domain = format!("gui/{}", nix::unistd::getuid());
        for label in [
            "com.agent-ix.ix-build-metrics",
            "com.agent-ix.ix-build-prune",
        ] {
            let target = format!("{domain}/{label}");
            if Command::new("launchctl")
                .args(["print", &target])
                .output()?
                .status
                .success()
                && !Command::new("launchctl")
                    .args(["bootout", &target])
                    .status()?
                    .success()
            {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "launchctl monitoring bootout failed",
                ));
            }
            let status = Command::new("launchctl")
                .args(["bootstrap", &domain])
                .arg(
                    home.join("Library/LaunchAgents")
                        .join(format!("{label}.plist")),
                )
                .status()?;
            if !status.success() {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "launchctl monitoring bootstrap failed",
                ));
            }
        }
    } else {
        for args in [
            vec!["--user", "daemon-reload"],
            vec![
                "--user",
                "enable",
                "--now",
                "ix-build-metrics.timer",
                "ix-build-prune.timer",
            ],
        ] {
            if !Command::new("systemctl").args(args).status()?.success() {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "systemd monitoring installation failed",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod monitoring_tests {
    use super::*;
    /// Trace: FR-015-AC-5, FR-015-AC-10. Provenance: SWM-431.
    #[test]
    fn metric_file_migration_keeps_a_single_collector_file() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("locked-build.prom"),
            b"ix_build_busy_units 1\n",
        )?;
        migrate_metric_file(root.path())?;
        assert_eq!(
            std::fs::read(root.path().join("ix-build.prom"))?,
            b"ix_build_busy_units 1\n"
        );
        std::fs::write(root.path().join("locked-build.prom"), b"stale")?;
        migrate_metric_file(root.path())?;
        migrate_metric_file(root.path())?;
        assert_eq!(
            std::fs::read(root.path().join("ix-build.prom"))?,
            b"ix_build_busy_units 1\n"
        );
        assert!(!root.path().join("locked-build.prom").exists());
        Ok(())
    }
    /// Trace: FR-015-AC-5, FR-015-AC-10
    #[test]
    fn telemetry_install_uses_detected_exporter_path_and_safe_schedules() -> Result<()> {
        assert_eq!(
            textfile_from_arguments(&[
                "/usr/bin/node_exporter".into(),
                "--collector.textfile.directory=/home/peter/textfile".into()
            ]),
            Some(PathBuf::from("/home/peter/textfile"))
        );
        assert_eq!(
            textfile_from_arguments(&[
                "node_exporter".into(),
                "--collector.textfile.directory".into(),
                "/tmp/textfile".into()
            ]),
            Some(PathBuf::from("/tmp/textfile"))
        );
        assert!(textfile_from_arguments(&["node_exporter".into()]).is_none());
        assert!(
            textfile_from_arguments(&[
                "other".into(),
                "--collector.textfile.directory=/tmp".into()
            ])
            .is_none()
        );
        let root = tempfile::tempdir()?;
        let home = root.path().join("home & space");
        let textfile = root.path().join("canonical textfile");
        install_monitoring(
            &home,
            &home.join("bin/ix-build"),
            &home.join("state"),
            &home.join("policy.json"),
            &textfile,
        )?;
        if cfg!(target_os = "macos") {
            for file in [
                "com.agent-ix.ix-build-metrics.plist",
                "com.agent-ix.ix-build-prune.plist",
            ] {
                let path = home.join("Library/LaunchAgents").join(file);
                let text = std::fs::read_to_string(&path)?;
                assert!(!text.contains('@'));
                assert!(text.contains("<key>Nice</key><integer>19</integer>"));
                assert!(text.contains("LowPriorityIO"));
                assert!(text.contains("Background"));
                let checked = Command::new("plutil").arg("-lint").arg(path).output()?;
                assert!(
                    checked.status.success(),
                    "{}",
                    String::from_utf8_lossy(&checked.stdout)
                );
            }
            let metric = std::fs::read_to_string(
                home.join("Library/LaunchAgents/com.agent-ix.ix-build-metrics.plist"),
            )?;
            assert!(metric.contains(&format!("{}/ix-build.prom", textfile.display())));
            assert!(metric.contains("<integer>15</integer>"));
        } else {
            let metric = std::fs::read_to_string(
                home.join(".config/systemd/user/ix-build-metrics.service"),
            )?;
            assert!(metric.contains("Nice=19"));
            assert!(metric.contains("IOSchedulingClass=idle"));
            assert!(metric.contains("CPUWeight=10"));
            assert!(metric.contains("TimeoutStartSec=10"));
            assert!(metric.contains(&format!("\"{}/ix-build.prom\"", textfile.display())));
        }
        Ok(())
    }
}
