// SPDX-License-Identifier: MIT
//! Private, bounded, atomic state protected by one operating-system lock.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::State;

const STATE_LIMIT: u64 = 32 * 1024 * 1024;
const METRICS_FILE: &str = "metrics.json";

pub(crate) struct Store {
    pub path: PathBuf,
    pub config_path: PathBuf,
}

impl Store {
    pub(crate) fn open(path: &Path, config_path: &Path) -> Result<Self> {
        match fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::new(
                ErrorCode::State,
                "state directory must be private and not a symlink",
            ));
        }
        Ok(Self {
            path: path.canonicalize()?,
            config_path: config_path.canonicalize()?,
        })
    }

    pub(crate) fn open_existing(path: &Path, config_path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::new(ErrorCode::State, "metrics state path does not exist")
            } else {
                error.into()
            }
        })?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::new(
                ErrorCode::State,
                "state directory must be private and not a symlink",
            ));
        }
        let state_path = path.join("state.json");
        let state_metadata = fs::symlink_metadata(&state_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::new(ErrorCode::State, "metrics state file does not exist")
            } else {
                error.into()
            }
        })?;
        if !state_metadata.is_file() {
            return Err(Error::new(ErrorCode::State, "state file is not regular"));
        }
        Ok(Self {
            path: path.canonicalize()?,
            config_path: config_path.canonicalize()?,
        })
    }

    pub(crate) fn transaction<T>(
        &self,
        operation: impl FnOnce(&mut State, &Config) -> Result<T>,
    ) -> Result<T> {
        self.reject_link("lock")?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.path.join("lock"))?;
        lock.lock()?;
        let config = Config::read(&self.config_path)?;
        self.reject_link("state.json")?;
        let mut state = match File::open(self.path.join("state.json")) {
            Ok(file) => {
                if file.metadata()?.len() > STATE_LIMIT {
                    return Err(Error::new(ErrorCode::State, "state exceeds size limit"));
                }
                serde_json::from_reader(file.take(STATE_LIMIT))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State {
                schema_version: 1,
                store_id: random_id()?,
                config_path: self.config_path.clone(),
                host: config.host.clone(),
                boot_time: sysinfo::System::boot_time(),
                next_sequence: 1,
                next_event: 1,
                pruned_events: 0,
                metrics: crate::metrics::MetricsState::default(),
                requests: Vec::new(),
                events: Vec::new(),
            },
            Err(error) => return Err(error.into()),
        };
        if state.schema_version != 1
            || state.config_path != self.config_path
            || state.host != config.host
        {
            return Err(Error::new(
                ErrorCode::State,
                "unsupported schema or different host/configuration",
            ));
        }
        self.reject_link(METRICS_FILE)?;
        let metrics_path = self.path.join(METRICS_FILE);
        state.metrics = match File::open(&metrics_path) {
            Ok(file) => {
                if file.metadata()?.len() > STATE_LIMIT {
                    return Err(Error::new(
                        ErrorCode::State,
                        "metrics state exceeds size limit",
                    ));
                }
                serde_json::from_reader(file.take(STATE_LIMIT))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::metrics::MetricsState::default()
            }
            Err(error) => return Err(error.into()),
        };
        if state.boot_time != sysinfo::System::boot_time() {
            state.metrics.reset_after_reboot();
        }
        state
            .metrics
            .reconcile_events(&state.events, state.next_event)?;
        state.validate(&config)?;
        let before = serde_json::to_vec(&state)?;
        let metrics_before = serde_json::to_vec(&state.metrics)?;
        let previous_pruned = state.pruned_events;
        let output = operation(&mut state, &config)?;
        state
            .metrics
            .reconcile_events(&state.events, state.next_event)?;
        let monotonic = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let clock = u64::try_from(monotonic.tv_sec)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000)
            .saturating_add(u64::try_from(monotonic.tv_nsec).unwrap_or(0) / 1_000_000);
        if state
            .requests
            .iter()
            .any(|request| request.phase == crate::model::Phase::Queued)
        {
            state.metrics.checkpoint_active(clock)?;
        } else {
            state.metrics.close_block(clock)?;
        }
        state.validate(&config)?;
        let bytes = serde_json::to_vec(&state)?;
        let metrics_bytes = serde_json::to_vec(&state.metrics)?;
        if bytes == before
            && metrics_bytes == metrics_before
            && self.path.join("state.json").try_exists()?
        {
            return Ok(output);
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > STATE_LIMIT {
            return Err(Error::new(
                ErrorCode::State,
                "state size limit reached; prune terminal requests",
            ));
        }
        if previous_pruned != state.pruned_events {
            self.write_atomic(METRICS_FILE, &metrics_bytes)?;
            self.write_atomic("state.json", &bytes)?;
        } else {
            self.write_atomic("state.json", &bytes)?;
            self.write_atomic(METRICS_FILE, &metrics_bytes)?;
        }
        File::open(&self.path)?.sync_all()?;
        Ok(output)
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let next = format!("{name}.next");
        self.reject_link(&next)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(self.path.join(&next))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(self.path.join(next), self.path.join(name))?;
        Ok(())
    }

    fn reject_link(&self, name: &str) -> Result<()> {
        match fs::symlink_metadata(self.path.join(name)) {
            Ok(metadata) if !metadata.is_file() => Err(Error::new(
                ErrorCode::State,
                "state member is not a regular file",
            )),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn random_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
