// SPDX-License-Identifier: MIT
//! Keep memory-only scopes alive through the command's final memory.events read.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::model::CommandOutcome;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OomEvidence {
    pub outcome: CommandOutcome,
    pub oom_kill_count: u64,
    pub memory_peak_bytes: Option<u64>,
}

impl OomEvidence {
    pub(crate) fn command_status(&self) -> Result<std::process::ExitStatus> {
        let raw = match self.outcome {
            CommandOutcome {
                code: Some(code @ 0..=255),
                signal: None,
            } => code * 256,
            CommandOutcome {
                code: None,
                signal: Some(signal @ 1..=127),
            } => signal,
            _ => {
                return Err(Error::new(
                    ErrorCode::State,
                    "invalid saved scope command outcome",
                ));
            }
        };
        Ok(std::process::ExitStatus::from_raw(raw))
    }
}

pub(crate) fn run(evidence: &Path, command: &[String]) -> Result<i32> {
    let program = command
        .first()
        .ok_or_else(|| Error::new(ErrorCode::Request, "empty scope command"))?;
    let status = Command::new(program).args(&command[1..]).status()?;
    // This small parent is still in the scope after the workload exits. systemd
    // cannot collect its cgroup until the final kernel counters are durable.
    let events = crate::process::cgroup_memory_events_file(std::process::id())
        .ok_or_else(|| Error::new(ErrorCode::State, "scope memory.events path unavailable"))?;
    let oom_kill_count = crate::process::oom_kill_count_from_file(&events)
        .ok_or_else(|| Error::new(ErrorCode::State, "scope oom_kill counter unavailable"))?;
    let memory_peak_bytes = events
        .parent()
        .and_then(|parent| std::fs::read_to_string(parent.join("memory.peak")).ok())
        .and_then(|value| value.trim().parse().ok());
    crate::writable::publish_file(
        evidence,
        &serde_json::to_vec(&OomEvidence {
            outcome: CommandOutcome {
                code: status.code(),
                signal: status.signal(),
            },
            oom_kill_count,
            memory_peak_bytes,
        })?,
        true,
    )?;
    // The supervisor uses the saved typed outcome, including signals ignored by
    // Rust itself (SIGPIPE) and realtime signals not in nix's Signal enum.
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Provenance: SWM-435. Trace: FR-005-AC-4
    #[test]
    fn saved_outcomes_preserve_exit_codes_sigpipe_and_realtime_signals() -> Result<()> {
        for (json, code, signal) in [
            (
                r#"{"outcome":{"code":75,"signal":null},"oom_kill_count":0,"memory_peak_bytes":null}"#,
                Some(75),
                None,
            ),
            (
                r#"{"outcome":{"code":null,"signal":13},"oom_kill_count":0,"memory_peak_bytes":null}"#,
                None,
                Some(13),
            ),
            (
                r#"{"outcome":{"code":null,"signal":34},"oom_kill_count":0,"memory_peak_bytes":null}"#,
                None,
                Some(34),
            ),
        ] {
            let evidence: OomEvidence = serde_json::from_str(json)?;
            let status = evidence.command_status()?;
            assert_eq!(status.code(), code);
            assert_eq!(status.signal(), signal);
        }
        Ok(())
    }
}
