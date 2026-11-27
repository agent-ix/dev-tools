// SPDX-License-Identifier: MIT
//! Real admitted-command wrapper selection using a Rust-only fixture.
use std::path::Path;
use std::process::Command;

/// Provenance: SWM-409; no host configuration or real compiler/cache is modified.
#[test]
#[ignore = "wrapper acceptance requires IX_BUILD_KACHE_WRAPPER_FIXTURE example binary"]
fn kache_wrapper_admitted_commands_default_on_off_unhealthy_and_remote_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    let source = std::env::var_os("IX_BUILD_KACHE_WRAPPER_FIXTURE")
        .ok_or("IX_BUILD_KACHE_WRAPPER_FIXTURE required")?;
    let root = tempfile::tempdir()?;
    let build = root.path().join("fixture-build");
    std::fs::copy(&source, &build)?;
    let binary = env!("CARGO_BIN_EXE_locked-build");
    for (name, enabled, mode, status, active) in [
        ("kache-good", true, "local", "enabled", true),
        ("kache-good", false, "local", "disabled", false),
        ("kache-broken", true, "local", "unhealthy", false),
        ("kache-wrong", true, "local", "unhealthy", false),
        ("kache-hang", true, "local", "unhealthy", false),
        ("kache-remote-down", true, "both", "enabled", true),
    ] {
        let fixture = root.path().join(name);
        std::fs::copy(&source, &fixture)?;
        let case = tempfile::tempdir_in(root.path())?;
        let state = case.path().join("state");
        let policy = case.path().join("policy.json");
        // Policy init measures resources without selecting or launching a wrapper.
        let initialized = Command::new(binary)
            .env("HOME", case.path())
            .env_remove("IX_HOME")
            .env_remove("XDG_STATE_HOME")
            .arg("--state")
            .arg(&state)
            .arg("--config")
            .arg(&policy)
            .args(["policy", "init"])
            .output()?;
        assert!(
            initialized.status.success(),
            "{}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        let mut settings: serde_json::Value = serde_json::from_slice(&std::fs::read(&policy)?)?;
        settings["cargo_build_jobs"] = 4.into();
        settings["rust_test_threads"] = 4.into();
        settings["cache"] = serde_json::json!({"enabled":enabled,"mode":mode,"executable":fixture});
        std::fs::write(&policy, serde_json::to_vec(&settings)?)?;
        let start = std::time::Instant::now();
        let output = Command::new(binary)
            .env("HOME", case.path())
            .env_remove("IX_HOME")
            .env_remove("XDG_STATE_HOME")
            .arg("--state")
            .arg(&state)
            .arg("--config")
            .arg(&policy)
            .args([
                "run", "--class", "point", "--ticket", "SWM-409", "--units", "4", "--",
            ])
            .arg(&build)
            .env_remove("LOCKED_BUILD_LEASE")
            .env_remove("LOCKED_BUILD_STATE")
            .env_remove("LOCKED_BUILD_CONFIG")
            .env("KACHE_LOCAL_ONLY", "1")
            .env("RUSTC_WRAPPER", "/missing/inherited-wrapper")
            .output()?;
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let snapshot: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.join("state.json"))?)?;
        let records = snapshot["requests"].as_array().ok_or("requests missing")?;
        let request = records.last().ok_or("request missing")?;
        let id = request["id"].as_str().ok_or("request id missing")?;
        let compiled: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.join(format!("{id}.stdout")))?)?;
        assert_eq!(compiled["compiled"], true);
        assert_eq!(
            compiled["local_only"],
            if mode == "local" { "1" } else { "false" }
        );
        assert_eq!(
            compiled["wrapper"].as_str().is_some_and(|p| !p.is_empty()),
            active
        );
        assert_eq!(compiled["jobs"], "4");
        assert_eq!(compiled["threads"], "4");
        if name == "kache-hang" {
            assert!(start.elapsed().as_secs() < 5);
        }
        let lease = &records.last().ok_or("request missing")?["lease_record"];
        assert_eq!(lease["cache_wrapper_status"], status);
        assert_eq!(lease["cache_wrapper"].as_str().is_some(), active);
        assert!(!Path::new(&fixture).join("config.toml").exists());
    }
    Ok(())
}
