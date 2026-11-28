// SPDX-License-Identifier: MIT
//! Private host-wide fallback paths, independent of lane HOME and TMPDIR.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostNamespace {
    pub boot_id: String,
    pub pid_namespace: String,
}

pub(crate) fn install_host_namespace(shared: &Path) -> Result<()> {
    let (Some(boot_id), Some(pid_namespace)) =
        (crate::process::boot_id(), crate::process::pid_namespace()?)
    else {
        return Ok(());
    };
    let path = shared.join("host-namespace.json");
    let marker = HostNamespace {
        boot_id,
        pid_namespace,
    };
    publish_host_namespace(&path, &marker)
}

fn publish_host_namespace(path: &Path, marker: &HostNamespace) -> Result<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    match fs::read(path) {
        Ok(bytes) => {
            let existing: HostNamespace = serde_json::from_slice(&bytes)?;
            if existing.boot_id == marker.boot_id {
                return Ok(());
            }
            publish_file(path, &serde_json::to_vec(marker)?, true)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            publish_file(path, &serde_json::to_vec(marker)?, false)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn is_installed_host_namespace(
    shared: &Path,
    boot_id: Option<&str>,
    namespace: Option<&str>,
) -> bool {
    fs::read(shared.join("host-namespace.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HostNamespace>(&bytes).ok())
        .is_some_and(|marker| {
            Some(marker.boot_id.as_str()) == boot_id
                && Some(marker.pid_namespace.as_str()) == namespace
        })
}

pub(crate) fn host_root(home: Option<&Path>) -> PathBuf {
    let root = home.map_or_else(|| PathBuf::from(".ix"), |home| home.join(".ix"));
    existing_root(root.join("ix-build"), root.join("locked-build"))
}

/// Keep policy on durable storage, recovering the former temporary-file alias.
/// Trace: FR-011-AC-7. Provenance: SWM-444.
pub(crate) fn durable_policy(path: &Path, state: &Path) -> Result<PathBuf> {
    let temporary = state.join("policy.json");
    let is_link = fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink());
    let mut migrated = false;
    if path.try_exists()? {
        if is_link {
            // Resolve relative paths before moving the file's directory.
            let config = crate::config::Config::read(&path.canonicalize()?)?;
            publish_file(path, &serde_json::to_vec_pretty(&config)?, true)?;
            migrated = true;
        } else {
            crate::config::Config::read(path)?;
        }
    } else if temporary.try_exists()? {
        let config = crate::config::Config::read(&temporary)?;
        publish_file(path, &serde_json::to_vec_pretty(&config)?, is_link)?;
        migrated = true;
    } else if is_link {
        // The old /tmp target can disappear at reboot. Allow first-use policy
        // creation to publish a durable regular file in its place.
        fs::remove_file(path)?;
    }
    if migrated && temporary.exists() && temporary.canonicalize()? != path.canonicalize()? {
        // Already-running schedulers may retain the old temporary config path.
        // Keep their next admission read on the same durable policy.
        let next = state.join(format!(".policy-alias-{}.next", crate::store::random_id()?));
        std::os::unix::fs::symlink(path.canonicalize()?, &next)?;
        let result = fs::rename(&next, &temporary);
        let _ = fs::remove_file(next);
        result?;
    }
    Ok(path.to_owned())
}

// Until install-tools publishes the new alias, use the existing namespace.
// Never open a second queue or lose the recorded rollout owner during upgrade.
fn existing_root(new: PathBuf, old: PathBuf) -> PathBuf {
    if !new.exists() && old.exists() {
        old
    } else {
        new
    }
}

pub(crate) fn publish_file(path: &Path, bytes: &[u8], force: bool) -> Result<bool> {
    use std::io::Write;
    publish_file_with(path, force, |file| file.write_all(bytes))
}

fn publish_file_with(
    path: &Path,
    force: bool,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> Result<bool> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "policy path has no parent"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let next = parent.join(format!(".policy-{}.next", crate::store::random_id()?));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&next)?;
        write(&mut file)?;
        file.sync_all()?;
        if force {
            fs::rename(&next, path)?;
            return Ok(true);
        }
        match fs::hard_link(&next, path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error.into()),
        }
    })();
    let _ = fs::remove_file(next);
    result
}

pub(crate) fn shared_root() -> Result<PathBuf> {
    shared_root_at(
        &Path::new("/tmp").join(format!("ix-build-{}", nix::unistd::geteuid())),
        &legacy_shared_path(),
    )
}

fn shared_root_at(new: &Path, legacy: &Path) -> Result<PathBuf> {
    if new.try_exists()? && legacy.try_exists()? && new.canonicalize()? != legacy.canonicalize()? {
        return Err(Error::new(
            ErrorCode::State,
            "both namespace paths exist; preserving both",
        ));
    }
    let path = existing_root(new.to_owned(), legacy.to_owned());
    // Only the exact, owned legacy-store alias created by install-tools is valid.
    if fs::symlink_metadata(&path).is_ok_and(|m| m.is_symlink()) {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.uid() != nix::unistd::geteuid().as_raw()
            || path.canonicalize()? != legacy.canonicalize()?
        {
            return Err(Error::new(
                ErrorCode::State,
                "unexpected shared namespace alias",
            ));
        }
        return private_directory(legacy);
    }
    private_directory(&path)
}

pub(crate) fn shared_path() -> PathBuf {
    existing_root(
        Path::new("/tmp").join(format!("ix-build-{}", nix::unistd::geteuid())),
        legacy_shared_path(),
    )
}

pub(crate) fn legacy_shared_path() -> PathBuf {
    Path::new("/tmp").join(format!("locked-build-{}", nix::unistd::geteuid()))
}

/// Publish a new name without moving any queue, ledger, lock inode or lease path.
/// Trace: FR-011-AC-7, FR-005-AC-4. Provenance: SWM-431.
pub(crate) fn migrate_namespace(new: &Path, old: &Path) -> Result<()> {
    if !old.try_exists()? {
        return Ok(());
    }
    let old = private_directory(old)?;
    if fs::symlink_metadata(new).is_ok() {
        if new.canonicalize()? == old {
            return Ok(());
        }
        return Err(Error::new(
            ErrorCode::State,
            "both namespace paths exist; preserving both",
        ));
    }
    install_alias(new, &old)?;
    Ok(())
}

pub(crate) fn shared_state(default: &Path, shared: &Path) -> Result<PathBuf> {
    match install_alias(default, shared) {
        Ok(()) => {}
        Err(error) if access_denied(&error) => {}
        Err(error) => return Err(error.into()),
    }
    Ok(shared.to_owned())
}

pub(crate) fn install_alias(default: &Path, shared: &Path) -> std::io::Result<()> {
    let parent = default
        .parent()
        .ok_or_else(|| std::io::Error::other("alias has no parent"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    match std::os::unix::fs::symlink(shared, default) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(default)?;
            if metadata.uid() == nix::unistd::geteuid().as_raw()
                && metadata.is_symlink()
                && fs::read_link(default)? == shared
            {
                Ok(())
            } else {
                Err(std::io::Error::other(
                    "automatic state/policy alias conflicts with existing path; preserving it",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn private_directory(path: &Path) -> Result<PathBuf> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(Error::new(
            ErrorCode::State,
            "shared fallback must be an owned mode 0700 directory, not a symlink",
        ));
    }
    path.canonicalize().map_err(Into::into)
}

/// Probe actual sandbox access, rather than Unix mode bits alone.
pub(crate) fn probe_directory(path: &Path) -> std::io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    static NEXT_PROBE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_PROBE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = path.join(format!(".write-probe-{}-{sequence}", std::process::id()));
    let probe = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    drop(probe);
    fs::remove_file(path)
}

pub(crate) fn access_denied(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
        || error.kind() == std::io::ErrorKind::ReadOnlyFilesystem
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trace: FR-011-AC-7. Provenance: SWM-444.
    #[test]
    fn policy_publication_keeps_complete_old_or_new_bytes_after_write_failure() -> Result<()> {
        use std::io::{Read, Write};
        use std::os::unix::fs::MetadataExt;
        for existing in [false, true] {
            let root = tempfile::tempdir()?;
            let path = root.path().join("policy.json");
            let mut previous = if existing {
                fs::write(&path, b"complete old policy")?;
                Some(std::fs::File::open(&path)?)
            } else {
                None
            };
            let result = publish_file_with(&path, true, |file| {
                file.write_all(b"partial new")?;
                Err(std::io::Error::other("injected write failure"))
            });
            assert!(result.is_err());
            if existing {
                assert_eq!(fs::read(&path)?, b"complete old policy");
            } else {
                assert!(!path.exists());
            }
            assert_eq!(fs::read_dir(root.path())?.count(), usize::from(existing));
            publish_file(&path, b"complete new policy", true)?;
            assert_eq!(fs::read(&path)?, b"complete new policy");
            if let Some(file) = &mut previous {
                assert_ne!(file.metadata()?.ino(), fs::metadata(&path)?.ino());
                let mut old_bytes = Vec::new();
                file.read_to_end(&mut old_bytes)?;
                assert_eq!(old_bytes, b"complete old policy");
            }
            assert_eq!(fs::read_dir(root.path())?.count(), 1);
        }
        Ok(())
    }

    /// Trace: FR-011-AC-7. Provenance: SWM-444.
    #[test]
    fn policy_publication_rename_failure_preserves_destination_and_removes_staging() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("policy.json");
        fs::create_dir(&path)?;
        fs::write(path.join("sentinel"), b"preserved")?;
        assert!(publish_file(&path, b"complete policy", true).is_err());
        assert_eq!(fs::read(path.join("sentinel"))?, b"preserved");
        assert_eq!(fs::read_dir(root.path())?.count(), 1);
        assert_eq!(fs::read_dir(&path)?.count(), 1);
        Ok(())
    }

    /// Trace: FR-011-AC-7. Provenance: SWM-444.
    #[test]
    fn policy_migration_with_state_in_the_config_directory_keeps_a_regular_file() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("old.json");
        fs::write(&source, br#"{"host":"operator"}"#)?;
        let directory = root.path().join(".ix/ix-build");
        fs::create_dir_all(&directory)?;
        let path = directory.join("policy.json");
        std::os::unix::fs::symlink(&source, &path)?;
        durable_policy(&path, &directory)?;
        assert!(!fs::symlink_metadata(&path)?.is_symlink());
        assert_eq!(crate::config::Config::read(&path)?.host, "operator");
        Ok(())
    }

    /// Trace: FR-011-AC-7. Provenance: SWM-431.
    #[test]
    fn private_directory_returns_canonical_tmp_path() -> Result<()> {
        let root = tempfile::Builder::new()
            .prefix("ix-build-canonical-")
            .tempdir_in("/tmp")?;
        let path = root.path().join("state");
        let resolved = private_directory(&path)?;
        assert_eq!(resolved, std::fs::canonicalize(&path)?);
        #[cfg(target_os = "macos")]
        assert!(resolved.starts_with("/private/tmp"));
        Ok(())
    }

    /// Trace: FR-011-AC-7, FR-005-AC-4. Provenance: SWM-431.
    #[test]
    fn namespace_migration_preserves_live_lock_state_and_ledger() -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir()?;
        let old = private_directory(&root.path().join("locked-build"))?;
        let new = root.path().join("ix-build");
        fs::write(old.join("state.json"), b"running lease")?;
        fs::write(old.join("policy-ledger.json"), b"audited policy")?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(old.join("lock"))?;
        lock.lock()?;
        let inode = fs::metadata(old.join("state.json"))?.ino();
        migrate_namespace(&new, &old)?;
        migrate_namespace(&new, &old)?;
        assert_eq!(shared_root_at(&new, &old)?, old);
        assert_eq!(fs::read(new.join("state.json"))?, b"running lease");
        assert_eq!(fs::read(new.join("policy-ledger.json"))?, b"audited policy");
        assert_eq!(fs::metadata(new.join("state.json"))?.ino(), inode);
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(new.join("lock"))?;
        assert!(contender.try_lock().is_err());
        fs::write(old.join("state.json"), b"completed lease")?;
        assert_eq!(fs::read(new.join("state.json"))?, b"completed lease");
        let conflict = private_directory(&root.path().join("conflict"))?;
        fs::write(conflict.join("state.json"), b"other queue")?;
        assert!(migrate_namespace(&conflict, &old).is_err());
        assert!(shared_root_at(&conflict, &old).is_err());
        assert_eq!(fs::read(conflict.join("state.json"))?, b"other queue");
        Ok(())
    }

    /// Trace: FR-005-AC-4, FR-011-AC-7
    #[test]
    fn host_namespace_marker_is_stable_within_a_boot() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("host-namespace.json");
        publish_host_namespace(
            &path,
            &HostNamespace {
                boot_id: "boot-a".into(),
                pid_namespace: "pid:[host]".into(),
            },
        )?;
        publish_host_namespace(
            &path,
            &HostNamespace {
                boot_id: "boot-a".into(),
                pid_namespace: "pid:[sandbox]".into(),
            },
        )?;
        assert!(is_installed_host_namespace(
            root.path(),
            Some("boot-a"),
            Some("pid:[host]")
        ));
        assert!(!is_installed_host_namespace(
            root.path(),
            Some("boot-a"),
            Some("pid:[sandbox]")
        ));
        publish_host_namespace(
            &path,
            &HostNamespace {
                boot_id: "boot-b".into(),
                pid_namespace: "pid:[new-host]".into(),
            },
        )?;
        assert!(is_installed_host_namespace(
            root.path(),
            Some("boot-b"),
            Some("pid:[new-host]")
        ));
        Ok(())
    }

    /// Provenance: SWM-380. Trace: FR-011-AC-7
    #[test]
    fn concurrent_policy_creation_publishes_one_complete_file() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("policy.json");
        let first_path = path.clone();
        let first =
            std::thread::spawn(move || publish_file(&first_path, b"first-complete-policy", false));
        let second = publish_file(&path, b"second-complete-policy", false)?;
        let first = first
            .join()
            .map_err(|_| Error::new(ErrorCode::State, "publisher panicked"))??;
        assert_ne!(first, second);
        let bytes = fs::read(path)?;
        assert!(bytes == b"first-complete-policy" || bytes == b"second-complete-policy");
        assert_eq!(fs::read_dir(root.path())?.count(), 1);
        Ok(())
    }

    /// Provenance: SWM-374. Trace: FR-011-AC-7
    #[test]
    fn host_paths_ignore_lane_ix_home() {
        assert_eq!(
            host_root(Some(Path::new("/home/u"))),
            Path::new("/home/u/.ix/ix-build")
        );
        assert_eq!(host_root(None), Path::new(".ix/ix-build"));
    }

    /// Provenance: SWM-374. Trace: FR-011-AC-7
    #[test]
    fn fallback_rejects_symlinks_and_public_directories() -> Result<()> {
        let root = tempfile::tempdir()?;
        let link = root.path().join("link");
        std::os::unix::fs::symlink(root.path(), &link)?;
        assert!(private_directory(&link).is_err());
        let public = root.path().join("public");
        fs::create_dir(&public)?;
        fs::set_permissions(&public, fs::Permissions::from_mode(0o755))?;
        assert!(private_directory(&public).is_err());
        let private = private_directory(&root.path().join("private"))?;
        assert_eq!(fs::metadata(private)?.permissions().mode() & 0o777, 0o700);
        Ok(())
    }
}
