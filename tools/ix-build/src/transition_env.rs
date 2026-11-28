// SPDX-License-Identifier: MIT
//! Transition reads prefer the new spelling and warn once for each legacy name.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

pub(crate) fn read<T>(name: &str, lookup: impl FnMut(&str) -> Option<T>) -> Option<T> {
    read_with(name, lookup, warn)
}

fn read_with<T>(
    name: &str,
    mut lookup: impl FnMut(&str) -> Option<T>,
    mut warning: impl FnMut(&str, &str),
) -> Option<T> {
    if let Some(value) = lookup(name) {
        return Some(value);
    }
    let suffix = name.strip_prefix("IX_BUILD_")?;
    let legacy = format!("LOCKED_BUILD_{suffix}");
    let value = lookup(&legacy)?;
    warning(&legacy, name);
    Some(value)
}

pub(crate) fn os(name: &str) -> Option<OsString> {
    read(name, |key| std::env::var_os(key))
}

pub(crate) fn string(name: &str) -> Option<String> {
    os(name).map(|value| value.to_string_lossy().into_owned())
}

fn warn(legacy: &str, new: &str) {
    static WARNED: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    if let Ok(mut warned) = WARNED.get_or_init(Mutex::default).lock()
        && warned.insert(legacy.to_owned())
    {
        let _ = writeln!(
            std::io::stderr().lock(),
            "ix-build: warning: {legacy} is deprecated; use {new}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trace: FR-011-AC-7. Provenance: SWM-431.
    #[test]
    fn every_transition_variable_prefers_new_and_warns_only_on_legacy_use() {
        for suffix in [
            "STATE",
            "CONFIG",
            "OWNER",
            "TICKET",
            "CLASS",
            "REAL_CARGO",
            "LEASE",
        ] {
            let new = format!("IX_BUILD_{suffix}");
            let old = format!("LOCKED_BUILD_{suffix}");
            let mut warnings = Vec::new();
            assert_eq!(
                read_with(
                    &new,
                    |key| (key == old).then_some("old"),
                    |legacy, replacement| warnings
                        .push((legacy.to_owned(), replacement.to_owned()))
                ),
                Some("old")
            );
            assert_eq!(warnings, [(old, new.clone())]);
            warnings.clear();
            assert_eq!(
                read_with(
                    &new,
                    |key| Some(if key == new { "new" } else { "old" }),
                    |legacy, replacement| warnings
                        .push((legacy.to_owned(), replacement.to_owned()))
                ),
                Some("new")
            );
            assert!(warnings.is_empty());
            assert_eq!(
                read_with::<&str>(
                    &new,
                    |_| None,
                    |_, _| panic!("absent variables must not warn")
                ),
                None
            );
        }
    }
}
