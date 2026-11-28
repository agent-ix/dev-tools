// SPDX-License-Identifier: MIT
//! Default-on, per-request wrapper selection with a bounded read-only binary check.
use crate::config::CachePolicy;
use crate::model::CacheWrapperStatus;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) struct Prepared {
    policy: CachePolicy,
    binary: Option<PathBuf>,
    status: CacheWrapperStatus,
}

impl Prepared {
    /// Runs before taking the command-start transaction lock.
    pub(crate) fn discover(policy: &CachePolicy) -> Self {
        if !policy.enabled {
            return Self::prepare(policy, None);
        }
        let binary = policy.executable_or(crate::sample::kache::executable);
        Self::prepare(policy, binary.as_deref())
    }

    fn prepare(policy: &CachePolicy, binary: Option<&Path>) -> Self {
        let (binary, status) = if !policy.enabled {
            (None, CacheWrapperStatus::Disabled)
        } else if let Some(binary) = binary {
            match binary
                .canonicalize()
                .ok()
                .filter(|p| p.is_absolute() && healthy(p))
            {
                Some(path) => (Some(path), CacheWrapperStatus::Enabled),
                None => (None, CacheWrapperStatus::Unhealthy),
            }
        } else {
            (None, CacheWrapperStatus::Missing)
        };
        Self {
            policy: policy.clone(),
            binary,
            status,
        }
    }

    pub(crate) fn apply(
        &self,
        command: &mut Command,
        current: &CachePolicy,
    ) -> (Option<PathBuf>, CacheWrapperStatus) {
        current.apply(command);
        let status = if !current.enabled {
            CacheWrapperStatus::Disabled
        } else if current != &self.policy {
            CacheWrapperStatus::PolicyChanged
        } else {
            self.status
        };
        let binary = (status == CacheWrapperStatus::Enabled)
            .then_some(self.binary.as_deref())
            .flatten();
        // An empty value also disables a wrapper inherited from Cargo config.
        command.env(
            "RUSTC_WRAPPER",
            binary.map(Path::as_os_str).unwrap_or_default(),
        );
        (binary.map(Path::to_path_buf), status)
    }
}

fn healthy(binary: &Path) -> bool {
    version(binary, Duration::from_millis(500)).is_some_and(|version| {
        version
            .trim()
            .strip_prefix("kache ")
            .is_some_and(|v| semver::Version::parse(v).is_ok())
    })
}

fn version(binary: &Path, timeout: Duration) -> Option<String> {
    let mut child = Command::new(binary)
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut reaped = false;
    let result = (|| {
        let mut stdout = child.stdout.take()?;
        nix::fcntl::fcntl(
            &stdout,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .ok()?;
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    if let Some(status) = child.try_wait().ok()? {
                        reaped = true;
                        return status
                            .success()
                            .then(|| String::from_utf8(output).ok())
                            .flatten();
                    }
                }
                Ok(count) => {
                    output.extend_from_slice(&buffer[..count]);
                    if output.len() > 4096 {
                        return None;
                    }
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return None,
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    if !reaped {
        let _ = child.kill();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    result
}

#[cfg(test)]
mod tests;
