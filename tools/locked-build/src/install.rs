// SPDX-License-Identifier: MIT
//! Idempotent host setup and pinned tool-version checks.

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};

const TOOL_MANIFEST: &str = include_str!("../tool-versions.json");
const MANIFEST_VERSION: u32 = 1;

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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct InstalledRecord {
    schema_version: u32,
    locked_build_version: String,
    policy_path: PathBuf,
    cargo_shim: PathBuf,
    pinned_tools: Vec<ToolPin>,
}

/// Installs the host policy and cargo shim next to an already installed binary.
pub(crate) fn install_tools(
    home: &Path,
    current_exe: &Path,
    search_path: Option<&OsStr>,
) -> Result<()> {
    install_tools_with_measurement(home, current_exe, search_path, Config::measured_default)
}

fn install_tools_with_measurement(
    home: &Path,
    current_exe: &Path,
    search_path: Option<&OsStr>,
    measure_policy: impl FnOnce() -> Result<Config>,
) -> Result<()> {
    let manifest = read_manifest(TOOL_MANIFEST)?;
    let installed_path = home.join(".local/bin/locked-build");
    let installed_bin_dir = installed_path.parent().ok_or_else(|| {
        Error::new(
            ErrorCode::Configuration,
            "installed binary path has no parent",
        )
    })?;
    std::fs::create_dir_all(installed_bin_dir)?;
    copy_file_if_changed(current_exe, &installed_path)?;
    let config_dir = home.join(".config/locked-build");
    let data_dir = home.join(".local/share/dev-tools");
    let shim_dir = home.join(".local/lib/locked-build/shims");
    create_private_dir(&config_dir)?;
    create_private_dir(&data_dir)?;
    create_private_dir(&shim_dir)?;

    let policy_path = config_dir.join("policy.json");
    let mut policy = if policy_path.exists() {
        Config::read(&policy_path)?
    } else {
        let mut policy = measure_policy()?;
        policy.host = policy.host.to_ascii_lowercase();
        policy.real_cargo = discover_executable(search_path, "cargo", current_exe);
        policy.target_root = Some(home.join(".cache/locked-build/targets"));
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
    write_if_changed(&policy_path, &serde_json::to_vec_pretty(&policy)?, 0o600)?;

    let shim_path = shim_dir.join("cargo");
    install_symlink(&installed_path, &shim_path)?;
    let installed_record = InstalledRecord {
        schema_version: MANIFEST_VERSION,
        locked_build_version: env!("CARGO_PKG_VERSION").to_owned(),
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

    println!("installed locked-build {}", env!("CARGO_PKG_VERSION"));
    println!("policy: {}", policy_path.display());
    println!("cargo shim: {}", shim_path.display());
    println!("binary: {}", installed_path.display());
    println!("version record: {}", record_path.display());
    Ok(())
}

/// Compares installed command versions against the checked-in pinned manifest.
pub(crate) fn check_install(manifest_path: Option<&Path>) -> Result<i32> {
    let manifest = match manifest_path {
        Some(path) => read_manifest(&std::fs::read_to_string(path)?)?,
        None => read_manifest(TOOL_MANIFEST)?,
    };
    let mut mismatches = 0_u32;
    let home = std::env::var_os("HOME").map(PathBuf::from);
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
                || tool.version.as_ref().is_some_and(|version| {
                    version.is_empty()
                        || !version.chars().any(|character| character.is_ascii_digit())
                })
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
    let executable = resolve_tool_executable(&pin.executable, search_path, home)
        .ok_or_else(|| "unavailable=not-found".to_owned())?;
    let argument = if pin.version.is_some() {
        "--version"
    } else {
        "--help"
    };
    let output = Command::new(executable)
        .arg(argument)
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
    let Some(version) = pin.version.as_deref() else {
        return Ok("present".to_owned());
    };
    let observed = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if version_matches(&pin.name, version, &observed) {
        Ok(version.to_owned())
    } else {
        Err(format!("observed={observed}"))
    }
}

fn version_matches(name: &str, expected: &str, observed: &str) -> bool {
    if observed == expected {
        return true;
    }
    observed
        .strip_prefix(name)
        .map(str::trim_start)
        .and_then(|version_and_details| version_and_details.split_whitespace().next())
        == Some(expected)
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
                            && candidate.file_name() != Some(OsStr::new("locked-build"))
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
        if !candidate.is_file() {
            return None;
        }
        let resolved = candidate.canonicalize().ok()?;
        if name == "cargo" && resolved.file_name() == Some(OsStr::new("locked-build")) {
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
    if current.lines().any(|existing| existing == line) {
        return Ok(());
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
mod tests {
    use super::*;

    #[test]
    fn checked_in_pins_have_hand_written_version_requirements_and_executable_names() -> Result<()> {
        let manifest = read_manifest(TOOL_MANIFEST)?;
        let expected = [
            ("locked-build", "locked-build", Some("0.1.0")),
            ("quire", "quire", Some("0.53.3")),
            ("quoin", "quoin", Some("0.36.10")),
            ("ix-board", "ix-board", Some("0.12.0")),
            ("ix-trace-run", "ix-trace-run", None),
            ("cargo", "cargo", Some("1.98.1")),
            ("rustc", "rustc", Some("1.98.1")),
        ];
        let actual: Vec<_> = manifest
            .tools
            .iter()
            .map(|tool| {
                (
                    tool.name.as_str(),
                    tool.executable.as_str(),
                    tool.version.as_deref(),
                )
            })
            .collect();
        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn presence_only_manifest_pin_passes_when_help_succeeds() -> Result<()> {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir()?;
        let executable = root.path().join("presence-tool");
        symlink(std::env::current_exe()?, &executable)?;
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
        };
        assert_eq!(
            check_tool(&missing, Some(&search_path), None),
            Err("unavailable=not-found".to_owned())
        );
        Ok(())
    }

    #[test]
    fn version_check_recognizes_cli_version_formats() {
        assert!(version_matches(
            "quire",
            "0.53.3",
            "quire 0.53.3 (engine 0.63.2)"
        ));
        assert!(version_matches("quoin", "0.36.10", "0.36.10"));
        assert!(!version_matches("quoin", "0.36.10", "quoin 0.36.10-rc.1"));
        assert!(!version_matches("quoin", "0.36.10", "quoin 0.36.10+local"));
        assert!(version_matches(
            "quoin",
            "0.36.10+local",
            "quoin 0.36.10+local"
        ));
        assert!(!version_matches("quoin", "0.36.10", "0.36.9"));
    }

    #[test]
    fn version_check_skips_the_shim_and_finds_real_cargo_after_it() -> Result<()> {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir()?;
        let shim_dir = root.path().join("shim");
        let cargo_dir = root.path().join("cargo");
        std::fs::create_dir_all(&shim_dir)?;
        std::fs::create_dir_all(&cargo_dir)?;
        let shim_target = root.path().join("locked-build");
        let rustup = cargo_dir.join("rustup");
        let real_cargo = cargo_dir.join("cargo");
        std::fs::write(&shim_target, b"shim")?;
        std::fs::write(&rustup, b"rustup")?;
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
        let destination = root.path().join("bin/locked-build");
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
        let destination = root.path().join("bin/locked-build");
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
        let current_exe = std::env::current_exe()?;

        install_tools_with_measurement(&home, &current_exe, None, || {
            let mut config: Config =
                serde_json::from_str(include_str!("../policy.example.json"))
                    .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
            config.host = "test-host".to_owned();
            config.capacity_units = 4;
            config.maximum_units = 4;
            config.validate()?;
            Ok(config)
        })?;

        let binary = home.join(".local/bin/locked-build");
        let policy_path = home.join(".config/locked-build/policy.json");
        let shim = home.join(".local/lib/locked-build/shims/cargo");
        let record = home.join(".local/share/dev-tools/installed.json");
        assert_eq!(std::fs::read(&binary)?, std::fs::read(&current_exe)?);
        assert_eq!(
            std::fs::metadata(&binary)?.permissions().mode() & 0o111,
            0o111
        );
        assert_eq!(std::fs::read_link(&shim)?, binary.canonicalize()?);
        let policy = Config::read(&policy_path)?;
        assert_eq!(policy.priority_classes.get("arch-test"), Some(&2));
        assert_eq!(policy.priority_classes.get("kani"), Some(&0));
        let profile = std::fs::read_to_string(home.join(".zshrc"))?;
        assert!(profile.contains(".local/lib/locked-build/shims"));
        let saved_record: InstalledRecord = serde_json::from_slice(&std::fs::read(&record)?)?;
        assert_eq!(saved_record.locked_build_version, env!("CARGO_PKG_VERSION"));

        let first_modified = [
            std::fs::metadata(&binary)?.modified()?,
            std::fs::metadata(&policy_path)?.modified()?,
            std::fs::metadata(&record)?.modified()?,
            std::fs::metadata(home.join(".zshrc"))?.modified()?,
        ];
        install_tools_with_measurement(&home, &current_exe, None, || {
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
