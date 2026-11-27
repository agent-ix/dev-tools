// SPDX-License-Identifier: MIT
//! Compile-time version proof; runtime environment and git availability cannot replace it.

/// Provenance: SWM-425; hand-written build inputs, with git deliberately unavailable at runtime.
#[test]
#[ignore = "build with IX_BUILD_COMMIT=1111111111111111111111111111111111111111 IX_BUILD_DIRTY=false"]
fn provenance_version_uses_compiled_commit_without_runtime_git()
-> Result<(), Box<dyn std::error::Error>> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_locked-build"))
        .arg("--version")
        .env("PATH", "/missing")
        .env(
            "IX_BUILD_COMMIT",
            "2222222222222222222222222222222222222222",
        )
        .env("IX_BUILD_DIRTY", "true")
        .output()?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    assert!(text.contains("commit 1111111111111111111111111111111111111111, dirty=false"));
    assert!(!text.contains("2222222222222222222222222222222222222222"));
    Ok(())
}

/// Provenance: SWM-425; invalid build input must remain visibly unverifiable.
#[test]
#[ignore = "build with IX_BUILD_COMMIT=invalid IX_BUILD_DIRTY=false"]
fn provenance_unknown_commit_is_dirty() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_locked-build"))
        .arg("--version")
        .env("PATH", "/missing")
        .output()?;
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)?.contains("commit unknown, dirty=true"));
    Ok(())
}
