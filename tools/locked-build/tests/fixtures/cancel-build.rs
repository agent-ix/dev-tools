// SPDX-License-Identifier: MIT
//! Keep a real Cargo compilation in its build step until cancellation.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = std::env::var_os("LOCKED_BUILD_CANCEL_FIXTURE") else {
        return Ok(());
    };
    let marker = std::env::var_os("LOCKED_BUILD_FIXTURE_MARKER").ok_or("marker missing")?;
    let mut child = std::process::Command::new(fixture)
        .args(["--exact", "subprocess_fixture", "--ignored", "--nocapture"])
        .arg("--skip")
        .arg(marker)
        .env("LOCKED_BUILD_FIXTURE_MODE", "cancel-compiler")
        .spawn()?;
    let status = child.wait()?;
    Err(format!("compilation fixture unexpectedly ended: {status}").into())
}
