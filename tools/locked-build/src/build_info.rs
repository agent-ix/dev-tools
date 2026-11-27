// SPDX-License-Identifier: MIT
//! Build-script values; no runtime repository inspection.

/// Commit captured by the build or unknown when unavailable.
pub(crate) const COMMIT: &str = env!("IX_BUILD_COMMIT");
/// Dirty or unverifiable source state captured by the build.
pub(crate) const DIRTY: &str = env!("IX_BUILD_DIRTY");
/// Human version including the exact compiled source provenance.
pub(crate) const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (commit ",
    env!("IX_BUILD_COMMIT"),
    ", dirty=",
    env!("IX_BUILD_DIRTY"),
    ")"
);
