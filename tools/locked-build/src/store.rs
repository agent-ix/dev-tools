// SPDX-License-Identifier: MIT
//! Private, bounded, atomic state protected by one operating-system lock.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::State;
use crate::process::{IdentityState, Processes};

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
        Ok(Self {
            path: validate_state_directory(path)?,
            config_path: config_path.canonicalize()?,
        })
    }

    pub(crate) fn open_existing(path: &Path, config_path: &Path) -> Result<Self> {
        let directory = validate_state_directory(path)?;
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
            path: directory,
            config_path: config_path.canonicalize()?,
        })
    }

    pub(crate) fn lock_metrics_export(&self) -> Result<File> {
        lock_metrics_export_at(&self.path)
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
        let mode_path = self.path.join("mode.json");
        self.reject_link("mode.json")?;
        let mut write_mode_after_request = false;
        let mode = match fs::read_to_string(&mode_path) {
            Ok(mode) => mode,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.reject_observed_store()?;
                write_mode_after_request = true;
                "\"admitted\"".to_owned()
            }
            Err(error) => return Err(error.into()),
        };
        if serde_json::from_str::<String>(&mode)? != "admitted" {
            return Err(Error::new(ErrorCode::Request, "store is in observed mode"));
        }
        self.reject_observed_store()?;
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
        let metrics_path = self.path.join(METRICS_FILE);
        state.metrics = load_metrics(&metrics_path);
        let metrics_before = serde_json::to_vec(&state.metrics)?;
        let metrics_identity_changed = state.metrics.bind_store(&state.store_id);
        if state.boot_time != sysinfo::System::boot_time() {
            let last_host_poll_at_ms = state
                .requests
                .iter()
                .filter(|request| request.phase == crate::model::Phase::Queued)
                .filter_map(|request| request.heartbeat_at_ms)
                .max();
            state.metrics.finish_for_reboot(
                sysinfo::System::boot_time().saturating_mul(1_000),
                last_host_poll_at_ms,
                config.poll_ms,
            )?;
        }
        let now = crate::runtime::now_ms()?;
        let clock = crate::runtime::clock_ms()?;
        let mut processes = None;
        state.metrics.expire_stale_block(
            &state.requests,
            now,
            clock,
            config.poll_ms,
            |request| {
                let Some(worker) = request.worker.as_ref() else {
                    return false;
                };
                let current_processes = processes.get_or_insert_with(Processes::read);
                matches!(
                    current_processes.identity(worker, &request.id),
                    IdentityState::Matching | IdentityState::Unavailable
                )
            },
        )?;
        state
            .metrics
            .reconcile_events(&state.events, state.next_event)?;
        state.validate(&config)?;
        let before = serde_json::to_vec(&state)?;
        let previous_pruned = state.pruned_events;
        let output = operation(&mut state, &config)?;
        if write_mode_after_request && !state.requests.is_empty() {
            self.write_atomic("mode.json", b"\"admitted\"")?;
        }
        state
            .metrics
            .reconcile_events(&state.events, state.next_event)?;
        let now = crate::runtime::now_ms()?;
        let clock = crate::runtime::clock_ms()?;
        if !state
            .requests
            .iter()
            .any(|request| request.phase == crate::model::Phase::Queued)
        {
            state.metrics.close_block(clock, now)?;
        }
        state.validate(&config)?;
        let bytes = serde_json::to_vec(&state)?;
        let metrics_bytes = serde_json::to_vec(&state.metrics)?;
        let state_changed = bytes != before || !self.path.join("state.json").try_exists()?;
        let metrics_changed = metrics_identity_changed || metrics_bytes != metrics_before;
        if !state_changed && !metrics_changed {
            return Ok(output);
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > STATE_LIMIT {
            return Err(Error::new(
                ErrorCode::State,
                "state size limit reached; prune terminal requests",
            ));
        }
        if previous_pruned != state.pruned_events {
            if metrics_changed {
                self.write_atomic(METRICS_FILE, &metrics_bytes)?;
            }
            if state_changed {
                self.write_atomic("state.json", &bytes)?;
            }
        } else {
            if state_changed {
                self.write_atomic("state.json", &bytes)?;
            }
            if metrics_changed && let Err(error) = self.write_atomic(METRICS_FILE, &metrics_bytes) {
                eprintln!("locked-build: metrics sidecar update failed: {error}");
            }
        }
        File::open(&self.path)?.sync_all()?;
        Ok(output)
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let next = format!("{name}.next");
        self.reject_link(&next)?;
        write_atomic_path(
            &self.path.join(name),
            &self.path.join(next),
            bytes,
            0o600,
            None,
        )
    }

    fn reject_observed_store(&self) -> Result<()> {
        self.reject_link("observe.json")?;
        if self.path.join("observe.json").exists() {
            let observed = fs::read(self.path.join("observe.json"))?;
            let value: serde_json::Value = serde_json::from_slice(&observed)?;
            if value.get("mode").and_then(serde_json::Value::as_str) == Some("observed") {
                return Err(Error::new(ErrorCode::Request, "store is in observed mode"));
            }
        }
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

pub(crate) fn lock_metrics_export_at(path: &Path) -> Result<File> {
    let lock_path = path.join("metrics-export.lock");
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(Error::new(
                ErrorCode::State,
                "metrics export lock is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        _ => {}
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    lock.lock()?;
    Ok(lock)
}

fn random_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(crate) fn validate_state_directory(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::new(ErrorCode::State, "state directory does not exist")
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
    path.canonicalize().map_err(Into::into)
}

pub(crate) fn write_atomic_path(
    target: &Path,
    temporary: &Path,
    bytes: &[u8],
    mode: u32,
    cancelled: Option<&AtomicBool>,
) -> Result<()> {
    for path in [target, temporary] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(Error::new(
                    ErrorCode::State,
                    "atomic state path is not a regular file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            _ => {}
        }
    }
    if temporary.exists() {
        fs::remove_file(temporary)?;
    }
    if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        return Err(Error::new(ErrorCode::Io, "atomic state write cancelled"));
    }
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err(Error::new(ErrorCode::Io, "atomic state write cancelled"));
        }
        fs::rename(temporary, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn load_metrics(path: &Path) -> crate::metrics::MetricsState {
    let result = (|| -> Result<crate::metrics::MetricsState> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(crate::metrics::MetricsState::default());
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() || metadata.len() > STATE_LIMIT {
            return Err(Error::new(ErrorCode::State, "invalid metrics sidecar file"));
        }
        let file = File::open(path)?;
        serde_json::from_reader(file.take(STATE_LIMIT)).map_err(Into::into)
    })();
    match result {
        Ok(metrics) => metrics,
        Err(error) => {
            eprintln!("locked-build: ignoring invalid metrics sidecar: {error}");
            crate::metrics::MetricsState::default()
        }
    }
}
