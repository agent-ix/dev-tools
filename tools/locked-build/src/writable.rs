// SPDX-License-Identifier: MIT
//! Private host-wide fallback paths, independent of lane HOME and TMPDIR.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};

// Same resolution as quoin-modules::paths::IxHome::resolve: nonempty
// IX_HOME wins (including relative paths), then HOME/.ix, then .ix.
pub(crate) fn ix_home(ix_home: Option<PathBuf>, home: Option<&Path>) -> PathBuf {
    ix_home
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| home.map_or_else(|| PathBuf::from(".ix"), |home| home.join(".ix")))
}

pub(crate) fn shared_root() -> Result<PathBuf> {
    private_directory(&Path::new("/tmp").join(format!("locked-build-{}", nix::unistd::geteuid())))
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

    /// Provenance: SWM-374. Trace: FR-011-AC-7
    #[test]
    fn ix_home_matches_quoin_resolution() {
        assert_eq!(
            ix_home(Some("/ix".into()), Some(Path::new("/home/u"))),
            Path::new("/ix")
        );
        assert_eq!(
            ix_home(Some("".into()), Some(Path::new("/home/u"))),
            Path::new("/home/u/.ix")
        );
        assert_eq!(ix_home(None, None), Path::new(".ix"));
        assert_eq!(
            ix_home(Some("relative".into()), None),
            Path::new("relative")
        );
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
