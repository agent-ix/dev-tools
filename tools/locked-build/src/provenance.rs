// SPDX-License-Identifier: MIT
//! Serialized, owner-pinned host installation provenance.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, ErrorCode, Result};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transfer {
    pub from: String,
    pub to: String,
    pub at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallRecord {
    pub schema_version: u32,
    pub owner: String,
    pub source_commit: String,
    pub artifact_sha256: String,
    pub installed_at_ms: u64,
    pub transfers: Vec<Transfer>,
}

pub(crate) struct InstallGuard {
    _lock: std::fs::File,
    pub path: PathBuf,
    pub record: InstallRecord,
}

impl InstallGuard {
    pub(crate) fn acquire(
        home: &Path,
        owner: &str,
        transfer_from: Option<&str>,
        source: &str,
        artifact: &Path,
        now: u64,
    ) -> Result<Self> {
        let invalid = |detail| Error::new(ErrorCode::Configuration, detail);
        if owner.trim().is_empty() || owner.len() > 256 {
            return Err(invalid("install-tools requires a nonempty rollout owner"));
        }
        if source.len() != 40 || !source.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid(
                "install-tools requires a full source commit; build with IX_BUILD_COMMIT",
            ));
        }
        let root = crate::writable::host_root(Some(home));
        std::fs::create_dir_all(&root)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(root.join("install.lock"))?;
        // Never let a second installer wait and later overwrite a fresh rollout.
        lock.try_lock().map_err(|_| {
            invalid("another install-tools writer is active; installation unchanged")
        })?;
        let path = root.join("install.json");
        let old = read_record(&path)?;
        let mut transfers = old
            .as_ref()
            .map(|record| record.transfers.clone())
            .unwrap_or_default();
        match old.as_ref() {
            Some(record) if record.owner != owner => {
                if transfer_from != Some(record.owner.as_str()) {
                    return Err(invalid(
                        "different rollout owner; use --transfer-from with the recorded owner to transfer ownership",
                    ));
                }
                transfers.push(Transfer {
                    from: record.owner.clone(),
                    to: owner.to_owned(),
                    at_ms: now,
                });
            }
            _ if transfer_from.is_some() => {
                return Err(invalid(
                    "ownership transfer does not match a different recorded owner",
                ));
            }
            _ => {}
        }
        Ok(Self {
            _lock: lock,
            path,
            record: InstallRecord {
                schema_version: 1,
                owner: owner.to_owned(),
                source_commit: source.to_owned(),
                artifact_sha256: artifact_hash(artifact)?,
                installed_at_ms: now,
                transfers,
            },
        })
    }

    pub(crate) fn publish(&self) -> Result<()> {
        crate::writable::publish_file(&self.path, &serde_json::to_vec_pretty(&self.record)?, true)
            .map(|_| ())
    }
}

pub(crate) fn read_record(path: &Path) -> Result<Option<InstallRecord>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let record: InstallRecord = serde_json::from_slice(&bytes)?;
            if record.schema_version != 1 {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "unsupported install provenance schema",
                ));
            }
            Ok(Some(record))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn artifact_hash(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65_536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn check(home: &Path) -> Result<(bool, String)> {
    let path = crate::writable::host_root(Some(home)).join("install.json");
    let Some(record) = read_record(&path)? else {
        return Ok((
            false,
            format!(
                "mismatch locked-build provenance missing={}",
                path.display()
            ),
        ));
    };
    let observed = artifact_hash(&home.join(".local/bin/locked-build"))?;
    let matches = observed == record.artifact_sha256;
    Ok((
        matches,
        format!(
            "{} locked-build provenance owner={} source_commit={} artifact_sha256={} observed_sha256={} installed_at_ms={}",
            if matches { "ok" } else { "mismatch" },
            record.owner,
            record.source_commit,
            record.artifact_sha256,
            observed,
            record.installed_at_ms
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = "1111111111111111111111111111111111111111";

    /// Provenance: SWM-403. Trace: FR-011-AC-7
    #[test]
    fn installs_serialize_pin_owner_and_record_explicit_transfers() -> Result<()> {
        let home = tempfile::tempdir()?;
        let binary = home.path().join("artifact");
        std::fs::write(&binary, b"abc")?;
        let first = InstallGuard::acquire(home.path(), "lead-a", None, SOURCE, &binary, 100)?;
        assert!(InstallGuard::acquire(home.path(), "lead-a", None, SOURCE, &binary, 101).is_err());
        first.publish()?;
        let path = first.path.clone();
        let before = std::fs::read(&path)?;
        drop(first);
        assert!(InstallGuard::acquire(home.path(), "lead-b", None, SOURCE, &binary, 102).is_err());
        assert!(
            InstallGuard::acquire(home.path(), "lead-b", Some("wrong"), SOURCE, &binary, 103)
                .is_err()
        );
        assert_eq!(std::fs::read(&path)?, before);
        let second =
            InstallGuard::acquire(home.path(), "lead-b", Some("lead-a"), SOURCE, &binary, 104)?;
        second.publish()?;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path)?)?,
            serde_json::json!({
                "schema_version":1,"owner":"lead-b","source_commit":SOURCE,
                "artifact_sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                "installed_at_ms":104,"transfers":[{"from":"lead-a","to":"lead-b","at_ms":104}]
            })
        );
        Ok(())
    }

    /// Provenance: SWM-403. Trace: FR-011-AC-7
    #[test]
    fn check_install_exposes_direct_binary_writes() -> Result<()> {
        let home = tempfile::tempdir()?;
        let binary = home.path().join(".local/bin/locked-build");
        std::fs::create_dir_all(
            binary
                .parent()
                .ok_or_else(|| Error::new(ErrorCode::Io, "missing parent"))?,
        )?;
        std::fs::write(&binary, b"abc")?;
        InstallGuard::acquire(home.path(), "rollout", None, SOURCE, &binary, 123)?.publish()?;
        assert_eq!(
            check(home.path())?,
            (
                true,
                format!(
                    "ok locked-build provenance owner=rollout source_commit={SOURCE} artifact_sha256=ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad observed_sha256=ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad installed_at_ms=123"
                )
            )
        );
        std::fs::write(&binary, b"changed")?;
        let (matches, line) = check(home.path())?;
        assert!(!matches);
        assert!(line.starts_with("mismatch locked-build provenance owner=rollout"));
        assert!(line.contains("observed_sha256="));
        Ok(())
    }
}
