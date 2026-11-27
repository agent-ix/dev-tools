// SPDX-License-Identifier: MIT
use super::*;

/// Provenance: SWM-409; missing, disabled and unhealthy binaries never refuse commands.
#[test]
fn kache_wrapper_unavailable_or_disabled_runs_without_wrapper()
-> Result<(), Box<dyn std::error::Error>> {
    let policy: CachePolicy = serde_json::from_str("{}")?;
    assert!(policy.enabled);
    for (binary, expected) in [
        (None, CacheWrapperStatus::Missing),
        (
            Some(Path::new("/missing/kache")),
            CacheWrapperStatus::Unhealthy,
        ),
        (
            Some(Path::new("/usr/bin/false")),
            CacheWrapperStatus::Unhealthy,
        ),
    ] {
        let prepared = Prepared::prepare(&policy, binary);
        let mut command = Command::new("/usr/bin/true");
        let (selected, reason) = prepared.apply(&mut command, &policy);
        assert!(selected.is_none());
        assert_eq!(reason, expected);
        assert!(
            command
                .get_envs()
                .any(|(k, v)| k == "RUSTC_WRAPPER" && v == Some(std::ffi::OsStr::new("")))
        );
        assert!(command.status()?.success());
    }
    let disabled = CachePolicy {
        enabled: false,
        ..Default::default()
    };
    let prepared = Prepared::prepare(&disabled, Some(Path::new("/missing/kache")));
    assert_eq!(prepared.status, CacheWrapperStatus::Disabled);
    Ok(())
}

/// Provenance: SWM-409; a concurrent policy change cannot use stale wrapper permission.
#[test]
fn kache_wrapper_policy_changes_disable_the_prepared_choice() {
    let original = CachePolicy::default();
    let prepared = Prepared {
        policy: original,
        binary: Some(PathBuf::from("/verified/kache")),
        status: CacheWrapperStatus::Enabled,
    };
    let mut changed = CachePolicy {
        mode: crate::config::CacheMode::Shared,
        ..Default::default()
    };
    let mut command = Command::new("/usr/bin/true");
    assert_eq!(
        prepared.apply(&mut command, &changed).1,
        CacheWrapperStatus::PolicyChanged
    );
    changed.enabled = false;
    assert_eq!(
        prepared.apply(&mut command, &changed).1,
        CacheWrapperStatus::Disabled
    );
}

/// Provenance: SWM-409; the version boundary refuses unrelated output and nonzero exit.
#[test]
fn kache_wrapper_binary_health_is_not_executable_presence_alone() {
    assert!(!healthy(Path::new("/usr/bin/true")));
    assert!(!healthy(Path::new("/usr/bin/false")));
    assert!(!healthy(Path::new("/missing/kache")));
}

/// Provenance: SWM-409; old persisted lease records retain an explicit unknown decision.
#[test]
fn kache_wrapper_legacy_lease_record_defaults_are_unrecorded()
-> Result<(), Box<dyn std::error::Error>> {
    let mut record = serde_json::to_value(crate::model::LeaseRecord::default())?;
    let object = record
        .as_object_mut()
        .ok_or("lease record is not an object")?;
    object.remove("cache_wrapper");
    object.remove("cache_wrapper_status");
    let restored: crate::model::LeaseRecord = serde_json::from_value(record)?;
    assert_eq!(
        restored.cache_wrapper_status,
        CacheWrapperStatus::Unrecorded
    );
    assert!(restored.cache_wrapper.is_none());
    Ok(())
}

/// Provenance: SWM-409; configured binary overrides PATH for wrappers and both sampler routes.
#[test]
fn kache_wrapper_policy_executable_takes_precedence_without_path_lookup() {
    let policy = CachePolicy {
        executable: Some(PathBuf::from("/configured/kache")),
        ..Default::default()
    };
    let looked_up = std::cell::Cell::new(false);
    let selected = policy.executable_or(|| {
        looked_up.set(true);
        Some(PathBuf::from("/path/kache"))
    });
    assert_eq!(selected.as_deref(), Some(Path::new("/configured/kache")));
    assert!(!looked_up.get());
}

/// Provenance: SWM-409; policy-relative executable/report paths have identical sampler semantics.
#[test]
fn kache_wrapper_policy_paths_resolve_from_the_configuration_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let mut policy = CachePolicy {
        executable: Some("bin/kache".into()),
        remote_report_file: Some("report.json".into()),
        remote_status_file: Some("doctor.json".into()),
        ..Default::default()
    };
    policy.resolve_paths(root.path())?;
    assert_eq!(policy.executable, Some(root.path().join("bin/kache")));
    assert_eq!(
        policy.remote_report_file,
        Some(root.path().join("report.json"))
    );
    assert_eq!(
        policy.remote_status_file,
        Some(root.path().join("doctor.json"))
    );
    Ok(())
}

/// Provenance: SWM-409 delta review; accepted mode spellings and roles have a complete policy matrix.
#[test]
fn kache_wrapper_mode_role_matrix_and_caller_local_override()
-> Result<(), Box<dyn std::error::Error>> {
    for mode in ["local", "shared", "both"] {
        for role in ["client", "server"] {
            let policy: CachePolicy =
                serde_json::from_value(serde_json::json!({"mode":mode,"cache_role":role}))?;
            let mut command = Command::new("/usr/bin/true");
            command.env_remove("KACHE_LOCAL_ONLY");
            policy.apply(&mut command);
            let expected = if mode == "local" { "true" } else { "false" };
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == "KACHE_LOCAL_ONLY"
                        && value == Some(std::ffi::OsStr::new(expected)))
            );
            assert_eq!(
                policy.mode_label(),
                if mode == "local" { "local" } else { "shared" }
            );
            assert_eq!(policy.role_label(), role);
        }
    }
    let policy = CachePolicy::default();
    for truthy in ["true", "TRUE", "1"] {
        let mut command = Command::new("/usr/bin/true");
        command.env("KACHE_LOCAL_ONLY", truthy);
        policy.apply(&mut command);
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "KACHE_LOCAL_ONLY"
                    && value == Some(std::ffi::OsStr::new(truthy)))
        );
    }
    Ok(())
}

/// Provenance: SWM-409 delta review; errors name the invalid policy field, and legacy files still load.
#[test]
fn kache_wrapper_config_loader_reports_bad_mode_and_role_and_loads_legacy_policy()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("policy.json");
    for (key, field, expected) in [
        ("mode", "cache.mode", "local or shared"),
        ("cache_role", "cache.cache_role", "client or server"),
    ] {
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"cache":{(key):"invalid"}}))?,
        )?;
        let error = crate::config::Config::read(&path)
            .err()
            .ok_or("invalid enum accepted")?;
        assert!(matches!(error.code, crate::error::ErrorCode::Configuration));
        assert!(error.to_string().contains(field), "{error}");
        assert!(error.to_string().contains(expected), "{error}");
    }
    let mut old = serde_json::to_value(crate::config::Config::default())?;
    old.as_object_mut()
        .ok_or("policy object missing")?
        .remove("cache");
    std::fs::write(&path, serde_json::to_vec(&old)?)?;
    let loaded = crate::config::Config::read(&path)?;
    assert!(loaded.cache.enabled);
    assert_eq!(loaded.cache.mode, crate::config::CacheMode::Local);
    assert_eq!(loaded.cache.cache_role, crate::config::CacheRole::Client);
    assert!(loaded.cache.executable.is_none());
    Ok(())
}
