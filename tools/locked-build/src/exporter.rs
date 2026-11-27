// SPDX-License-Identifier: MIT
//! Bounded collector and publication isolation, including fallback filesystem I/O.
use crate::error::{Error, ErrorCode, Result};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(8);
const COLLECT_DEADLINE: Duration = Duration::from_secs(6);

enum KillScope {
    Child,
    Group,
}

/// All store, previous-file and publication I/O runs inside this bounded child.
pub(crate) fn collect(state: &Path, config: &Path, output: Option<&Path>) -> Result<String> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--state")
        .arg(state)
        .arg("--config")
        .arg(config)
        .arg("metrics-publish")
        .process_group(0);
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    bounded_output(&mut command, DEADLINE, KillScope::Group)
}

/// Runs only in the publisher child, under the outer hard deadline.
pub(crate) fn publish(state: &Path, config: &Path, output: Option<&Path>) -> Result<String> {
    let previous = output
        .map(read_previous)
        .transpose()?
        .flatten()
        .unwrap_or_default();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--state")
        .arg(state)
        .arg("--config")
        .arg(config)
        .arg("metrics-collect");
    let observed = bounded_output(&mut command, COLLECT_DEADLINE, KillScope::Child);
    let failed = observed.is_err();
    let mut text = match observed {
        Ok(text) => text,
        Err(error) => {
            eprintln!("locked-build metrics: {error}");
            previous
                .lines()
                .filter(|line| {
                    !line.starts_with("ix_build_export_error{")
                        && !line.starts_with("# HELP ix_build_export_error ")
                        && !line.starts_with("# TYPE ix_build_export_error ")
                })
                .map(|line| format!("{line}\n"))
                .collect::<String>()
        }
    };
    // A fallback retains the last successful timestamp and acknowledgement.
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

fn read_previous(path: &Path) -> Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_file() => {
            return Err(Error::new(
                ErrorCode::Configuration,
                "metrics output must be a regular file",
            ));
        }
        Ok(_) => {}
    }
    let flags = nix::fcntl::OFlag::O_NONBLOCK | nix::fcntl::OFlag::O_NOFOLLOW;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits())
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::new(
            ErrorCode::Configuration,
            "metrics output changed to a non-regular file",
        ));
    }
    let mut previous = String::new();
    file.take(OUTPUT_LIMIT + 1).read_to_string(&mut previous)?;
    if u64::try_from(previous.len()).unwrap_or(u64::MAX) > OUTPUT_LIMIT {
        return Err(Error::new(
            ErrorCode::State,
            "previous metrics exceed output limit",
        ));
    }
    Ok(Some(previous))
}

fn bounded_output(command: &mut Command, timeout: Duration, scope: KillScope) -> Result<String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
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
                return Err(Error::new(ErrorCode::State, "metrics child unavailable"));
            }
            let text = receiver
                .recv_timeout(timeout.saturating_sub(started.elapsed()))
                .map_err(|_| Error::new(ErrorCode::State, "metrics output timed out"))??;
            if u64::try_from(text.len()).unwrap_or(u64::MAX) > OUTPUT_LIMIT {
                return Err(Error::new(ErrorCode::State, "metrics output limit"));
            }
            return Ok(text);
        }
        if started.elapsed() >= timeout {
            match scope {
                KillScope::Child => {
                    let _ = child.kill();
                }
                KillScope::Group => {
                    // The unreaped Child owns this freshly created group; its PID cannot be reused.
                    if let Ok(pid) = i32::try_from(child.id()) {
                        let _ = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(pid),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                    }
                }
            }
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Err(Error::new(
                ErrorCode::State,
                "metrics child exceeded hard timeout",
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
            KillScope::Child,
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        Ok(())
    }
    /// Trace: FR-015-AC-5
    #[test]
    fn telemetry_publisher_group_is_bounded_while_its_output_io_is_blocked() -> Result<()> {
        let root = tempfile::tempdir()?;
        let fifo = root.path().join("blocked");
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
        // A native reader blocks on the FIFO; the publisher supervisor must kill its group.
        let start = Instant::now();
        let result = bounded_output(
            Command::new("/bin/cat").arg(fifo).process_group(0),
            Duration::from_millis(100),
            KillScope::Group,
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        Ok(())
    }
    /// Trace: FR-015-AC-5, FR-015-AC-10
    #[test]
    fn telemetry_previous_output_rejects_fifo_before_opening() -> Result<()> {
        let root = tempfile::tempdir()?;
        let fifo = root.path().join("output.prom");
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
        assert!(read_previous(&fifo).is_err());
        Ok(())
    }
}
