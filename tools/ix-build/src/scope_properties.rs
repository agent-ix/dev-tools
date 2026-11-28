// SPDX-License-Identifier: MIT
//! Host capability cache shared by independent request supervisors (FR-005-AC-4).

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScopeProperties {
    Full,
    MemoryOnly,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedProbe {
    context: ProbeContext,
    properties: Option<ScopeProperties>,
}

#[derive(Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProbeContext {
    boot_id: Option<String>,
    pid_namespace: Option<String>,
    bus_address: Option<String>,
    runtime_directory: Option<std::path::PathBuf>,
}

pub(crate) fn cached(
    probe: impl FnMut(ScopeProperties) -> bool,
) -> Result<Option<ScopeProperties>> {
    cached_at(
        &crate::writable::shared_root()?,
        ProbeContext {
            boot_id: crate::process::boot_id(),
            pid_namespace: crate::process::pid_namespace()?,
            bus_address: std::env::var("DBUS_SESSION_BUS_ADDRESS").ok(),
            runtime_directory: std::env::var_os("XDG_RUNTIME_DIR").map(Into::into),
        },
        probe,
    )
}

fn cached_at(
    root: &Path,
    context: ProbeContext,
    mut probe: impl FnMut(ScopeProperties) -> bool,
) -> Result<Option<ScopeProperties>> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(root.join("scope-properties.lock"))?;
    lock.lock()?;
    let path = root.join("scope-properties-v1.json");
    if let Ok(bytes) = std::fs::read(&path)
        && let Ok(cached) = serde_json::from_slice::<CachedProbe>(&bytes)
        && cached.context == context
        && cached.properties.is_some()
    {
        return Ok(cached.properties);
    }
    let properties = [ScopeProperties::Full, ScopeProperties::MemoryOnly]
        .into_iter()
        .find(|properties| probe(*properties));
    // No successful probe can mean a missing user bus or a temporary manager
    // failure. It is not evidence of a host property capability; let a later
    // healthy caller retry rather than poison the host cache for this boot.
    if properties.is_none() {
        return Ok(None);
    }
    crate::writable::publish_file(
        &path,
        &serde_json::to_vec(&CachedProbe {
            context,
            properties,
        })?,
        true,
    )?;
    Ok(properties)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(boot: &str) -> ProbeContext {
        ProbeContext {
            boot_id: Some(boot.into()),
            ..Default::default()
        }
    }

    /// Provenance: SWM-435. Trace: FR-005-AC-4
    #[test]
    fn different_user_bus_cannot_reuse_a_healthy_callers_capability() -> Result<()> {
        let root = tempfile::tempdir()?;
        assert_eq!(
            cached_at(root.path(), context("boot-a"), |_| true)?,
            Some(ScopeProperties::Full)
        );
        let mut calls = Vec::new();
        let unavailable = ProbeContext {
            bus_address: Some("unix:path=/nonexistent-bus".into()),
            ..context("boot-a")
        };
        assert_eq!(
            cached_at(root.path(), unavailable, |properties| {
                calls.push(properties);
                false
            })?,
            None
        );
        assert_eq!(calls, [ScopeProperties::Full, ScopeProperties::MemoryOnly]);
        assert_eq!(
            cached_at(root.path(), context("boot-a"), |_| panic!(
                "healthy cache must survive"
            ))?,
            Some(ScopeProperties::Full)
        );
        Ok(())
    }

    /// Provenance: SWM-435. Trace: FR-005-AC-4
    #[test]
    fn probe_prefers_full_then_memory_only_and_caches_across_callers() -> Result<()> {
        for (supported, expected_calls) in [
            (Some(ScopeProperties::Full), vec![ScopeProperties::Full]),
            (
                Some(ScopeProperties::MemoryOnly),
                vec![ScopeProperties::Full, ScopeProperties::MemoryOnly],
            ),
        ] {
            let root = tempfile::tempdir()?;
            let mut calls = Vec::new();
            let actual = cached_at(root.path(), context("boot-a"), |properties| {
                calls.push(properties);
                Some(properties) == supported
            })?;
            assert_eq!(actual, supported);
            assert_eq!(calls, expected_calls);
            assert_eq!(
                cached_at(root.path(), context("boot-a"), |_| {
                    panic!("a new caller must reuse the host cache")
                })?,
                supported
            );
            let mut refreshed = 0;
            assert_eq!(
                cached_at(root.path(), context("boot-b"), |_| {
                    refreshed += 1;
                    true
                })?,
                Some(ScopeProperties::Full)
            );
            assert_eq!(refreshed, 1);
        }
        Ok(())
    }

    /// Provenance: SWM-435. Trace: FR-005-AC-4
    #[test]
    fn unavailable_user_bus_does_not_poison_later_healthy_callers() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut failed_calls = Vec::new();
        assert_eq!(
            cached_at(root.path(), context("boot-a"), |properties| {
                failed_calls.push(properties);
                false
            })?,
            None
        );
        assert_eq!(
            failed_calls,
            [ScopeProperties::Full, ScopeProperties::MemoryOnly]
        );
        let mut healthy_calls = Vec::new();
        assert_eq!(
            cached_at(root.path(), context("boot-a"), |properties| {
                healthy_calls.push(properties);
                true
            })?,
            Some(ScopeProperties::Full)
        );
        assert_eq!(healthy_calls, [ScopeProperties::Full]);
        Ok(())
    }
}
