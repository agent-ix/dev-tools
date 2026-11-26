// SPDX-License-Identifier: MIT
//! End-to-end defaults for the first policy-backed invocation.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn invoke(home: &Path, cwd: &Path, arguments: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_locked-build"))
        .args(arguments)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USER", "qa")
        .env_remove("LOCKED_BUILD_STATE")
        .env_remove("LOCKED_BUILD_CONFIG")
        .env_remove("LOCKED_BUILD_OWNER")
        .env_remove("LOCKED_BUILD_TICKET")
        .env_remove("LOCKED_BUILD_CLASS")
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

    let policy_path = home.join(".config/locked-build/policy.json");
    let state_dir = home.join(".local/state/locked-build");
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
        std::fs::metadata(home.join(".config/locked-build"))?
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
    std::fs::create_dir_all(home.join(".config/locked-build"))?;
    std::fs::create_dir_all(root.path().join("workspace"))?;
    std::fs::write(
        home.join(".config/locked-build/policy.json"),
        r#"{"maximum_units":0}"#,
    )?;
    let output = invoke(&home, &root.path().join("workspace"), &["status"])?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("maximum_units"));
    Ok(())
}
