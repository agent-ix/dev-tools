// SPDX-License-Identifier: MIT
//! Bounded collector isolation and last-good textfile publication.
use crate::error::{Error, ErrorCode, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(8);

/// The collector child can be killed without waiting for a filesystem probe.
pub(crate) fn collect(state: &Path, config: &Path, output: Option<&Path>) -> Result<String> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--state")
        .arg(state)
        .arg("--config")
        .arg(config)
        .arg("metrics-collect");
    let observed = bounded_output(&mut command, DEADLINE);
    let failed = observed.is_err();
    let mut text = match observed {
        Ok(text) => text,
        Err(error) => {
            eprintln!("locked-build metrics: {error}");
            output
                .and_then(|p| std::fs::File::open(p).ok())
                .and_then(|f| {
                    let mut previous = String::new();
                    f.take(OUTPUT_LIMIT).read_to_string(&mut previous).ok()?;
                    Some(
                        previous
                            .lines()
                            .filter(|line| {
                                !line.starts_with("ix_build_export_error{")
                                    && !line.starts_with("# HELP ix_build_export_error ")
                                    && !line.starts_with("# TYPE ix_build_export_error ")
                            })
                            .map(|line| format!("{line}\n"))
                            .collect::<String>(),
                    )
                })
                .unwrap_or_default()
        }
    };
    // A stale checkpoint is retained on error; freshness is never advanced by fallback.
    let host = text
        .lines()
        .find_map(|line| line.strip_prefix("ix_build_export_timestamp_seconds{"))
        .and_then(|line| line.split("host=\"").nth(1))
        .and_then(|line| line.split('"').next())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            crate::metrics::escape_label(
                &sysinfo::System::host_name()
                    .unwrap_or_else(|| "unknown".into())
                    .to_ascii_lowercase(),
            )
        });
    crate::telemetry::append_gauge(&mut text, "ix_build_export_error", &host, f64::from(failed));
    if let Some(output) = output {
        crate::metrics::write_atomic(output, &text)?;
        if !failed
            && let Some(checkpoint) = text
                .lines()
                .find_map(|line| line.strip_prefix("# locked-build-checkpoint "))
        {
            crate::metrics::write_atomic(&state.join("exported.json"), checkpoint)?;
        }
    }
    Ok(text)
}

fn bounded_output(command: &mut Command, timeout: Duration) -> Result<String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::new(ErrorCode::Io, "collector stdout missing"))?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut text = String::new();
        let result = stdout
            .take(OUTPUT_LIMIT + 1)
            .read_to_string(&mut text)
            .map(|_| text);
        let _ = sender.send(result);
    });
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(Error::new(ErrorCode::State, "collector unavailable"));
            }
            let text = receiver
                .recv_timeout(timeout.saturating_sub(started.elapsed()))
                .map_err(|_| Error::new(ErrorCode::State, "collector output timed out"))??;
            if u64::try_from(text.len()).unwrap_or(u64::MAX) > OUTPUT_LIMIT {
                return Err(Error::new(ErrorCode::State, "collector output limit"));
            }
            return Ok(text);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Err(Error::new(
                ErrorCode::State,
                "collector exceeded hard timeout",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Trace: FR-015-AC-5
    #[test]
    fn telemetry_collector_is_killed_at_its_hard_timeout() -> Result<()> {
        let start = Instant::now();
        let result = bounded_output(
            Command::new("/bin/sleep").arg("30"),
            Duration::from_millis(100),
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        Ok(())
    }
}
