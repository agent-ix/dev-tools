// SPDX-License-Identifier: MIT
//! End-to-end defaults for the first policy-backed invocation.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

/// Trace: FR-005-AC-4, FR-011-AC-7. Provenance: SWM-444.
#[test]
fn policy_set_memory_keys_print_before_after_and_preserve_invalid_inputs()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let policy = home.join(".ix/ix-build/policy.json");
    std::fs::create_dir_all(policy.parent().ok_or("missing policy parent")?)?;
    std::fs::write(&policy, br#"{"memory_budget_bytes":52000000000,"disk_reserve_bytes":100000000000,"memory_reserve_per_job_by_class":{"point":750000000},"memory_per_job_by_class":{"point":8000000000}}"#)?;
    for (key, value, expected) in [
        (
            "memory_budget_bytes",
            "53000000000",
            "memory_budget_bytes: 52000000000 -> 53000000000",
        ),
        (
            "memory_reserve_per_job_by_class",
            r#"{"point":800000000}"#,
            r#"memory_reserve_per_job_by_class: {"point":750000000} -> {"point":800000000}"#,
        ),
        (
            "memory_per_job_by_class",
            r#"{"point":7000000000}"#,
            r#"memory_per_job_by_class: {"point":8000000000} -> {"point":7000000000}"#,
        ),
    ] {
        let inode = std::fs::metadata(&policy)?.ino();
        let output = invoke(&home, root.path(), &["policy", "set", key, value])?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout)?.trim(), expected);
        assert_ne!(std::fs::metadata(&policy)?.ino(), inode);
    }
    let before = std::fs::read(&policy)?;
    let output = invoke(
        &home,
        root.path(),
        &["policy", "set", "memory_budget_bytes", "0"],
    )?;
    assert!(!output.status.success());
    assert_eq!(std::fs::read(&policy)?, before);
    Ok(())
}

/// Trace: FR-011-AC-7. Provenance: SWM-431.
#[test]
fn version_reports_the_new_binary_name() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_ix-build"))
        .arg("--version")
        .output()?;
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)?.starts_with("ix-build 0.2.0"));
    Ok(())
}

fn invoke(home: &Path, cwd: &Path, arguments: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_ix-build"))
        .args(arguments)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USER", "qa")
        .env_remove("IX_HOME")
        // Existing override isolates the integration fixture from host admission.
        .env("IX_BUILD_STATE", home.join(".ix/ix-build/state"))
        .env_remove("IX_BUILD_CONFIG")
        .env_remove("LOCKED_BUILD_CONFIG")
        .env_remove("IX_BUILD_OWNER")
        .env_remove("LOCKED_BUILD_OWNER")
        .env_remove("IX_BUILD_TICKET")
        .env_remove("LOCKED_BUILD_TICKET")
        .env_remove("IX_BUILD_CLASS")
        .env_remove("LOCKED_BUILD_CLASS")
        .env_remove("IX_BUILD_LEASE")
        .env_remove("LOCKED_BUILD_LEASE")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .output()
}

/// Trace: FR-011-AC-7
#[test]
fn first_run_generates_policy_runs_command_and_reuses_it_without_flags()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&cwd)?;

    let first = invoke(&home, &cwd, &["run", "--", "/usr/bin/true"])?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stderr = String::from_utf8_lossy(&first.stderr);
    assert_eq!(
        first_stderr
            .lines()
            .filter(|line| line.contains("Wrote default policy to"))
            .count(),
        1
    );

    let policy_path = home.join(".ix/ix-build/policy.json");
    let state_dir = home.join(".ix/ix-build/state");
    let policy_bytes = std::fs::read(&policy_path)?;
    let policy: Value = serde_json::from_slice(&policy_bytes)?;
    assert!(
        policy["capacity_units"]
            .as_u64()
            .is_some_and(|units| units > 0)
    );
    assert!(
        policy["memory_budget_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert_eq!(
        std::fs::metadata(home.join(".ix/ix-build"))?
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&policy_path)?.permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&state_dir)?.permissions().mode() & 0o777,
        0o700
    );

    let status = invoke(&home, &cwd, &["status"])?;
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let report: Value = serde_json::from_slice(&status.stdout)?;
    assert!(
        report["requests"][0]["owner"]
            .as_str()
            .is_some_and(|owner| owner.starts_with("qa@") && owner.ends_with(":workspace"))
    );
    assert_eq!(report["requests"][0]["ticket"], "-");

    let second = invoke(&home, &cwd, &["run", "--", "/usr/bin/true"])?;
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(!String::from_utf8_lossy(&second.stderr).contains("Wrote default policy to"));
    assert_eq!(std::fs::read(&policy_path)?, policy_bytes);

    let shown = invoke(&home, &cwd, &["policy", "show"])?;
    assert!(shown.status.success());
    let shown_policy: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(shown_policy["host"], policy["host"]);

    let mut edited = policy;
    edited["host"] = Value::String("manual".to_owned());
    std::fs::write(&policy_path, serde_json::to_vec(&edited)?)?;
    let regenerated = invoke(&home, &cwd, &["policy", "init", "--force"])?;
    assert!(
        regenerated.status.success(),
        "{}",
        String::from_utf8_lossy(&regenerated.stderr)
    );
    let regenerated_policy: Value = serde_json::from_slice(&std::fs::read(&policy_path)?)?;
    assert_ne!(regenerated_policy["host"], "manual");
    Ok(())
}

#[test]
fn invalid_policy_error_names_the_field() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".ix/ix-build"))?;
    std::fs::create_dir_all(root.path().join("workspace"))?;
    std::fs::write(
        home.join(".ix/ix-build/policy.json"),
        r#"{"maximum_units":0}"#,
    )?;
    let output = invoke(&home, &root.path().join("workspace"), &["status"])?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("maximum_units"));
    Ok(())
}

/// Provenance: SWM-374. Trace: FR-011-AC-7
#[test]
fn nested_shim_passes_through_without_state_or_policy_writes()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("readonly-home");
    let shim = root.path().join("shim");
    let real = root.path().join("real");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&shim)?;
    std::fs::create_dir(&real)?;
    let policy = root.path().join("readonly-policy.json");
    std::fs::write(
        &policy,
        br#"{"host":"fixture","capacity_units":1,"maximum_units":1,"real_cargo":"/bin/echo"}"#,
    )?;
    std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o400))?;
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ix-build"), shim.join("cargo"))?;
    std::os::unix::fs::symlink("/bin/echo", real.join("cargo"))?;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500))?;
    for legacy in [false, true] {
        let mut cmd = Command::new(shim.join("cargo"));
        cmd.arg("passthrough-argument")
            .env("HOME", &home)
            .env_remove("IX_HOME")
            .env("IX_BUILD_CONFIG", &policy)
            .env("IX_BUILD_STATE", home.join("must-not-exist"))
            .env("PATH", std::env::join_paths([&shim, &real])?)
            .env_remove("IX_BUILD_LEASE")
            .env_remove("LOCKED_BUILD_LEASE")
            .env_remove("IX_BUILD_REAL_CARGO")
            .env_remove("LOCKED_BUILD_REAL_CARGO")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("RUST_TEST_THREADS");
        if legacy {
            cmd.env("CARGO_TARGET_DIR", "/work/target")
                .env("CARGO_BUILD_JOBS", "4")
                .env("RUST_TEST_THREADS", "4");
        } else {
            cmd.env("IX_BUILD_LEASE", "1");
        }
        let output = cmd.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"passthrough-argument\n");
        assert!(output.stderr.is_empty());
        assert_eq!(std::fs::read_dir(&home)?.count(), 0);
    }
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Provenance: SWM-392. Trace: FR-011-AC-7
#[test]
fn nested_shim_does_not_read_an_invalid_policy() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let shim = root.path().join("shim");
    let real = root.path().join("real");
    std::fs::create_dir(&shim)?;
    std::fs::create_dir(&real)?;
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ix-build"), shim.join("cargo"))?;
    std::os::unix::fs::symlink("/bin/echo", real.join("cargo"))?;
    let policy = root.path().join("invalid-policy.json");
    std::fs::write(
        &policy,
        "deliberately invalid; nested cargo must not read this",
    )?;
    let output = Command::new(shim.join("cargo"))
        .arg("nested-passthrough")
        .env("IX_BUILD_LEASE", "1")
        .env_remove("IX_BUILD_REAL_CARGO")
        .env_remove("LOCKED_BUILD_REAL_CARGO")
        .env("IX_BUILD_CONFIG", &policy)
        .env("IX_BUILD_STATE", root.path().join("must-not-exist"))
        .env("PATH", std::env::join_paths([shim, real])?)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"nested-passthrough\n");
    assert!(output.stderr.is_empty());
    assert!(!root.path().join("must-not-exist").exists());
    Ok(())
}

/// Provenance: SWM-392. Trace: FR-011-AC-7
#[test]
fn nested_missing_cargo_reports_failure_without_creating_a_store()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let shim = root.path().join("cargo");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ix-build"), &shim)?;
    let output = Command::new(&shim)
        .arg("test")
        .env("IX_BUILD_LEASE", "1")
        .env_remove("IX_BUILD_REAL_CARGO")
        .env_remove("LOCKED_BUILD_REAL_CARGO")
        .env("HOME", root.path().join("absent-home"))
        .env("IX_BUILD_STATE", root.path().join("absent-state"))
        .env("IX_BUILD_CONFIG", root.path().join("absent-policy"))
        .env("PATH", root.path())
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("ix-build: IB_"));
    assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

/// Provenance: SWM-374. Trace: FR-011-AC-7
#[test]
fn unnested_shim_is_admitted_with_readonly_policy() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let config_dir = home.join(".ix/ix-build");
    std::fs::create_dir_all(&config_dir)?;
    let policy_path = config_dir.join("policy.json");
    let policy = br#"{"host":"fixture","capacity_units":1,"maximum_units":1,"maximum_load":1000000,"memory_budget_bytes":8000000000,"real_cargo":"/bin/echo","cargo_build_jobs":4,"rust_test_threads":4}"#;
    std::fs::write(&policy_path, policy)?;
    std::fs::set_permissions(&policy_path, std::fs::Permissions::from_mode(0o400))?;
    let shim = root.path().join("cargo");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ix-build"), &shim)?;
    let output = Command::new(shim)
        .arg("--version")
        .env("HOME", &home)
        .env_remove("IX_HOME")
        .env_remove("IX_BUILD_CONFIG")
        .env_remove("LOCKED_BUILD_CONFIG")
        .env("IX_BUILD_STATE", config_dir.join("state"))
        .env_remove("IX_BUILD_LEASE")
        .env_remove("LOCKED_BUILD_LEASE")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_JOBS")
        .env_remove("RUST_TEST_THREADS")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state: Value =
        serde_json::from_slice(&std::fs::read(config_dir.join("state/state.json"))?)?;
    assert_eq!(state["requests"].as_array().map(Vec::len), Some(1));
    assert_eq!(state["requests"][0]["phase"], "succeeded");
    assert_eq!(state["events"][0]["state"], "queued");
    assert_eq!(state["events"][1]["state"], "running");
    assert_eq!(std::fs::read(&policy_path)?, policy);
    let shown = invoke(&home, root.path(), &["policy", "show"])?;
    assert!(shown.status.success());
    assert!(
        String::from_utf8_lossy(&shown.stderr)
            .contains(config_dir.join("state").to_str().ok_or("path")?)
    );
    let status = invoke(&home, root.path(), &["status"])?;
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        status["state_path"],
        config_dir
            .join("state")
            .canonicalize()?
            .to_str()
            .ok_or("path")?
    );
    Ok(())
}
