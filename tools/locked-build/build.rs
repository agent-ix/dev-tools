// SPDX-License-Identifier: MIT
//! Compile-time provenance only; the installed binary never invokes git for its version.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(std::env::var_os("CARGO_MANIFEST_DIR")?)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|s| s.trim().to_owned())
}

fn main() {
    // Dirty state includes all tracked and untracked repository files. Cargo treats
    // a missing watched file as changed, so refresh on every build rather than
    // cache a clean flag when an otherwise unwatched file changes. Never create it.
    if let Some(out_dir) = std::env::var_os("OUT_DIR") {
        println!(
            "cargo:rerun-if-changed={}",
            std::path::Path::new(&out_dir)
                .join("provenance-refresh")
                .display()
        );
    }
    println!("cargo:rerun-if-env-changed=IX_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=IX_BUILD_DIRTY");
    for path in ["build.rs", "src", "examples", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={path}");
    }
    for name in ["HEAD", "index", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(reference) = git(&["symbolic-ref", "--quiet", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    let commit = std::env::var("IX_BUILD_COMMIT")
        .ok()
        .or_else(|| git(&["rev-parse", "--verify", "HEAD"]))
        .filter(|v| (7..=64).contains(&v.len()) && v.bytes().all(|c| c.is_ascii_hexdigit()))
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "unknown".to_owned());
    let dirty = commit == "unknown"
        || std::env::var("IX_BUILD_DIRTY")
            .ok()
            .map(|v| v != "false")
            .or_else(|| {
                git(&["status", "--porcelain", "--untracked-files=normal"]).map(|s| !s.is_empty())
            })
            .unwrap_or(true);
    println!("cargo:rustc-env=IX_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=IX_BUILD_DIRTY={dirty}");
}
