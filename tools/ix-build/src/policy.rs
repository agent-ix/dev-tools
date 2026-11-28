// SPDX-License-Identifier: MIT
//! Validated policy writes with a backup and a hash-linked audit ledger.
use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Audit {
    at_ms: u64,
    key: String,
    before: String,
    after: String,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn check(shared: &Path, policy: &Path) -> Result<()> {
    let entries = read(shared)?;
    let observed = hash(&std::fs::read(policy).map_err(|error| {
        Error::new(
            ErrorCode::PolicyModified,
            format!("audited policy is unavailable: {error}"),
        )
    })?);
    if entries.last().is_none_or(|entry| entry.after != observed) {
        return Err(Error::new(
            ErrorCode::PolicyModified,
            "policy hash differs from its audit ledger; restore the audited policy backup before policy set",
        ));
    }
    Ok(())
}

pub(crate) fn baseline(shared: &Path, policy: &Path, reason: &str) -> Result<()> {
    let mut entries = read(shared)?;
    let after = hash(&std::fs::read(policy)?);
    if entries.last().is_some_and(|entry| entry.after == after) {
        return Ok(());
    }
    let before = entries
        .last()
        .map_or(String::new(), |entry| entry.after.clone());
    entries.push(Audit {
        at_ms: crate::runtime::now_ms()?,
        key: reason.into(),
        before,
        after,
    });
    ledger(shared, &entries)
}

pub(crate) fn set(store: &Store, key: &str, value: &str) -> Result<()> {
    let lock = store
        .try_lock_member("lock")?
        .ok_or_else(|| Error::new(ErrorCode::Transition, "policy writer is busy; retry"))?;
    let before = std::fs::read(&store.config_path)?;
    let mut entries = read(&store.path)?;
    if !entries.is_empty() {
        check(&store.path, &store.config_path)?;
    }
    let current: Config = serde_json::from_slice(&before)?;
    let mut encoded = serde_json::to_value(current)?;
    let fields = encoded
        .as_object_mut()
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "policy must be an object"))?;
    if !fields.contains_key(key) || key == "host" {
        return Err(Error::new(
            ErrorCode::Configuration,
            "unknown or immutable policy key",
        ));
    }
    let value: serde_json::Value = serde_json::from_str(value).unwrap_or_else(|_| value.into());
    fields.insert(key.into(), value);
    let next: Config = serde_json::from_value(encoded)?;
    next.validate()?;
    if next.disk_reserve_bytes < 100_000_000_000 {
        return Err(Error::new(
            ErrorCode::Configuration,
            "disk_reserve_bytes must be at least 100000000000 (host disk floor)",
        ));
    }
    let after = serde_json::to_vec_pretty(&next)?;
    let before_hash = hash(&before);
    let backup = store.path.join(format!("policy-backup-{before_hash}.json"));
    crate::writable::publish_file(&backup, &before, false)?;
    std::fs::File::open(&backup)?.sync_all()?;
    entries.push(Audit {
        at_ms: crate::runtime::now_ms()?,
        key: key.into(),
        before: before_hash,
        after: hash(&after),
    });
    crate::writable::publish_file(&store.config_path, &after, true)?;
    if let Err(error) = ledger(&store.path, &entries) {
        crate::writable::publish_file(&store.config_path, &before, true)?;
        return Err(error);
    }
    drop(lock);
    Ok(())
}

fn read(shared: &Path) -> Result<Vec<Audit>> {
    match std::fs::read(shared.join("policy-ledger.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}
fn ledger(shared: &Path, entries: &[Audit]) -> Result<()> {
    Store {
        path: shared.to_owned(),
        config_path: shared.join("policy.json"),
    }
    .write_atomic("policy-ledger.json", &serde_json::to_vec(entries)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_set_validates_backs_up_audits_and_detects_hand_edits() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("policy.json");
        let config = Config {
            disk_reserve_bytes: 100_000_000_000,
            ..Config::default()
        };
        std::fs::write(&path, serde_json::to_vec(&config)?)?;
        let store = Store::open(&root.path().join("state"), &path)?;
        baseline(&store.path, &path, "init")?;
        let before = std::fs::read(&path)?;
        for (key, value) in [
            ("unknown", "1"),
            ("max_bytes", "1"),
            ("disk_reserve_bytes", "99999999999"),
            ("maximum_units", "0"),
            ("capacity_units", "\"bad\""),
        ] {
            assert!(
                set(&store, key, value).is_err(),
                "{key} accepted invalid value"
            );
            assert_eq!(std::fs::read(&path)?, before);
        }
        set(&store, "disk_reserve_bytes", "120000000000")?;
        assert_eq!(Config::read(&path)?.disk_reserve_bytes, 120_000_000_000);
        assert_eq!(
            std::fs::read(
                store
                    .path
                    .join(format!("policy-backup-{}.json", hash(&before)))
            )?,
            before
        );
        let entries = read(&store.path)?;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].key, "disk_reserve_bytes");
        check(&store.path, &path)?;
        std::fs::write(&path, b"{}")?;
        assert_eq!(
            check(&store.path, &path).err().map(|e| e.code),
            Some(ErrorCode::PolicyModified)
        );
        assert_eq!(
            set(&store, "maximum_units", "2").err().map(|e| e.code),
            Some(ErrorCode::PolicyModified)
        );
        Ok(())
    }
}
