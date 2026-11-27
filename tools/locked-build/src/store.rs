// SPDX-License-Identifier: MIT
//! Private, bounded, atomic state protected by one operating-system lock.

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::model::{RequestId, RequestNotice, State, WorkerClaim};
use crate::process::{IdentityState, Processes};

const STATE_LIMIT: u64 = 32 * 1024 * 1024;
const METRICS_FILE: &str = "metrics.json";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified_ns: Option<u64>,
}

impl FileStamp {
    fn read(path: &Path) -> std::io::Result<Self> {
        let meta = fs::metadata(path)?;
        Ok(Self {
            device: meta.dev(),
            inode: meta.ino(),
            length: meta.len(),
            modified_ns: meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|age| u64::try_from(age.as_nanos()).ok()),
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot<S = State, M = crate::metrics::MetricsState> {
    state: S,
    metrics: M,
    source_state: FileStamp,
    source_metrics: Option<FileStamp>,
}

pub(crate) struct Store {
    pub path: PathBuf,
    pub config_path: PathBuf,
}

impl Store {
    pub(crate) fn try_lock_member(&self, name: &str) -> Result<Option<File>> {
        if name.contains('/') || name == "." || name == ".." {
            return Err(Error::new(ErrorCode::State, "invalid store member"));
        }
        self.reject_link(name)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.path.join(name))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    pub(crate) fn inherited_or_try_lock(&self, name: &str) -> Result<Option<File>> {
        use std::os::fd::AsFd;
        self.reject_link(name)?;
        let stdin: File = std::io::stdin().as_fd().try_clone_to_owned()?.into();
        if let (Ok(input), Ok(expected)) = (stdin.metadata(), fs::metadata(self.path.join(name)))
            && input.dev() == expected.dev()
            && input.ino() == expected.ino()
        {
            return match stdin.try_lock() {
                Ok(()) => Ok(Some(stdin)),
                Err(std::fs::TryLockError::WouldBlock) => Ok(None),
                Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
            };
        }
        self.try_lock_member(name)
    }

    pub(crate) fn write_claim(&self, claim: &WorkerClaim) -> Result<()> {
        self.write_atomic(
            &format!("{}.launch.json", claim.request_id.0),
            &serde_json::to_vec(claim)?,
        )
    }

    pub(crate) fn claim(&self, id: &RequestId) -> Result<Option<WorkerClaim>> {
        self.read_member(&format!("{}.launch.json", id.0), 16_384)
    }

    pub(crate) fn notice(&self, id: &RequestId) -> Result<Option<RequestNotice>> {
        self.read_member(&format!("{}.notice.json", id.0), 16_384)
    }

    fn read_member<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
        limit: u64,
    ) -> Result<Option<T>> {
        if name.contains('/') {
            return Err(Error::new(ErrorCode::State, "invalid store member"));
        }
        self.reject_link(name)?;
        match File::open(self.path.join(name)) {
            Ok(file) => {
                if file.metadata()?.len() > limit {
                    return Err(Error::new(
                        ErrorCode::State,
                        "store member exceeds size limit",
                    ));
                }
                Ok(Some(serde_json::from_reader(BufReader::new(
                    file.take(limit),
                ))?))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn snapshot(&self) -> Result<(State, Config)> {
        let config = Config::read(&self.config_path)?;
        if let Ok(Some(snapshot)) =
            self.read_member::<Snapshot>("snapshot.json", STATE_LIMIT * 2 + 4096)
            && FileStamp::read(&self.path.join("state.json")).ok().as_ref()
                == Some(&snapshot.source_state)
            && FileStamp::read(&self.path.join(METRICS_FILE)).ok() == snapshot.source_metrics
        {
            let mut state = snapshot.state;
            state.metrics = snapshot.metrics;
            if state.schema_version != 1 || state.host != config.host {
                return Err(Error::new(
                    ErrorCode::State,
                    "unsupported schema or different host/configuration",
                ));
            }
            state.validate(&config)?;
            return Ok((state, config));
        }
        // Legacy writers can still finish running commands during a handoff.
        // Read metrics first, then the atomic state file, and reconcile only
        // when all intervening events remain available (including pruning).
        for _ in 0..3 {
            let mut metrics = load_metrics(&self.path.join(METRICS_FILE));
            let history = metrics.has_history();
            let mut state = match self.read_member::<State>("state.json", STATE_LIMIT)? {
                Some(state) => state,
                None => State {
                    schema_version: 1,
                    store_id: "00000000000000000000000000000000".to_owned(),
                    config_path: self.config_path.clone(),
                    host: config.host.clone(),
                    boot_time: sysinfo::System::boot_time(),
                    boot_id: crate::process::boot_id(),
                    next_sequence: 1,
                    next_event: 1,
                    pruned_events: 0,
                    metrics: Default::default(),
                    requests: Vec::new(),
                    events: Vec::new(),
                },
            };
            if state.schema_version != 1 || state.host != config.host {
                return Err(Error::new(
                    ErrorCode::State,
                    "unsupported schema or different host/configuration",
                ));
            }
            metrics.bind_store(&state.store_id);
            if history && !metrics.can_reconcile(&state) {
                continue;
            }
            metrics.reconcile_events(&state.events, state.next_event)?;
            state.metrics = metrics;
            state.validate(&config)?;
            return Ok((state, config));
        }
        Err(Error::new(
            ErrorCode::State,
            "state/metrics snapshot changed during recovery",
        ))
    }

    pub(crate) fn repair_notices(&self, state: &State) {
        for request in &state.requests {
            let expected = request.notice(&state.store_id);
            if request.phase.terminal()
                && !self
                    .path
                    .join(format!("{}.notice.json", request.id.0))
                    .is_file()
            {
                continue;
            }
            if self.notice(&request.id).ok().flatten().as_ref() != Some(&expected)
                && let Err(error) =
                    serde_json::to_vec(&expected)
                        .map_err(Error::from)
                        .and_then(|bytes| {
                            self.write_atomic(&format!("{}.notice.json", request.id.0), &bytes)
                        })
            {
                eprintln!("locked-build: request notification unavailable: {error}");
            }
        }
    }

    fn publish_snapshot(&self, state: &State) -> Result<()> {
        let snapshot = Snapshot {
            state,
            metrics: &state.metrics,
            source_state: FileStamp::read(&self.path.join("state.json"))?,
            source_metrics: FileStamp::read(&self.path.join(METRICS_FILE)).ok(),
        };
        self.write_atomic("snapshot.json", &serde_json::to_vec(&snapshot)?)
    }
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
        self.reject_link("metrics-export.lock")?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.path.join("metrics-export.lock"))?;
        lock.lock()?;
        Ok(lock)
    }

    pub(crate) fn transaction<T>(
        &self,
        operation: impl FnOnce(&mut State, &Config) -> Result<T>,
    ) -> Result<T> {
        self.transaction_with_clock(
            || Ok((crate::runtime::now_ms()?, crate::runtime::clock_ms()?)),
            |state, config, _, _| operation(state, config),
        )
    }

    pub(crate) fn transaction_with_clock<T>(
        &self,
        mut read_clock: impl FnMut() -> Result<(u64, u64)>,
        operation: impl FnOnce(&mut State, &Config, u64, u64) -> Result<T>,
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
                serde_json::from_reader(BufReader::new(file.take(STATE_LIMIT)))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State {
                schema_version: 1,
                store_id: random_id()?,
                config_path: self.config_path.clone(),
                host: config.host.clone(),
                boot_time: sysinfo::System::boot_time(),
                boot_id: crate::process::boot_id(),
                next_sequence: 1,
                next_event: 1,
                pruned_events: 0,
                metrics: crate::metrics::MetricsState::default(),
                requests: Vec::new(),
                events: Vec::new(),
            },
            Err(error) => return Err(error.into()),
        };
        if state.schema_version != 1 || state.host != config.host {
            return Err(Error::new(
                ErrorCode::State,
                "unsupported schema or different host/configuration",
            ));
        }
        // The random store ID and host identify the queue, not the location
        // from which a lane happened to read identical host policy.
        let policy_path_changed = state.config_path != self.config_path;
        state.config_path = self.config_path.clone();
        let metrics_path = self.path.join(METRICS_FILE);
        state.metrics = load_metrics(&metrics_path);
        let metrics_before = serde_json::to_vec(&state.metrics)?;
        let metrics_identity_changed = state.metrics.bind_store(&state.store_id);
        if state.rebooted(
            crate::process::boot_id().as_deref(),
            sysinfo::System::boot_time(),
        ) {
            let last_host_poll_at_ms = state
                .requests
                .iter()
                .filter(|request| request.phase == crate::model::Phase::Queued)
                .filter_map(|request| request.heartbeat_at_ms)
                .max();
            state.metrics.finish_for_reboot(
                sysinfo::System::boot_time().saturating_mul(1_000),
                last_host_poll_at_ms,
                crate::config::POLL_INTERVAL_MS,
            )?;
        }
        let (now, clock) = read_clock()?;
        // Existing terminal records also expose the new diagnostic contract.
        // Unknown historical measurements stay explicitly unavailable.
        let mut terminal_diagnostics = std::collections::BTreeMap::new();
        for request in state
            .requests
            .iter_mut()
            .filter(|request| request.phase.terminal())
        {
            if request.trace.diagnostic.code.is_none()
                && request.phase != crate::model::Phase::Succeeded
            {
                request.trace.diagnostic = crate::diagnostics::for_request(
                    request,
                    &config,
                    request.ended_clock_ms.unwrap_or(clock),
                );
            }
            terminal_diagnostics.insert(request.id.0.clone(), request.trace.diagnostic.clone());
        }
        for event in state
            .events
            .iter_mut()
            .filter(|event| event.state.terminal())
        {
            if event.trace.diagnostic.code.is_none()
                && let Some(diagnostic) = terminal_diagnostics.get(&event.request_id.0)
            {
                event.trace.diagnostic = diagnostic.clone();
            }
        }
        let mut processes = None;
        state.metrics.expire_stale_block(
            &state.requests,
            now,
            clock,
            crate::config::POLL_INTERVAL_MS,
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
        let before_notices: std::collections::BTreeMap<_, _> = state
            .requests
            .iter()
            .map(|request| (request.id.0.clone(), request.notice(&state.store_id)))
            .collect();
        let previous_pruned = state.pruned_events;
        let (operation_now, operation_clock) = read_clock()?;
        let output = operation(&mut state, &config, operation_now, operation_clock)?;
        state
            .metrics
            .reconcile_events(&state.events, state.next_event)?;
        let (now, clock) = read_clock()?;
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
        let state_changed =
            policy_path_changed || bytes != before || !self.path.join("state.json").try_exists()?;
        let metrics_changed = metrics_identity_changed || metrics_bytes != metrics_before;
        if !state_changed && !metrics_changed {
            if let Err(error) = self.publish_snapshot(&state) {
                eprintln!("locked-build: status snapshot unavailable: {error}");
            }
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
        if let Err(error) = self.publish_snapshot(&state) {
            eprintln!("locked-build: status snapshot unavailable: {error}");
        }
        for request in &state.requests {
            let notice = request.notice(&state.store_id);
            if before_notices.get(&request.id.0) != Some(&notice)
                && let Err(error) =
                    serde_json::to_vec(&notice)
                        .map_err(Error::from)
                        .and_then(|bytes| {
                            self.write_atomic(&format!("{}.notice.json", request.id.0), &bytes)
                        })
            {
                eprintln!("locked-build: request notification unavailable: {error}");
            }
        }
        File::open(&self.path)?.sync_all()?;
        Ok(output)
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> Result<()> {
        // Notifications can be repaired concurrently with a canonical commit.
        // Each publication needs its own temporary inode before atomic rename.
        let next = format!("{name}.{}.next", random_id()?);
        self.reject_link(&next)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(self.path.join(&next))?;
        let result = (|| {
            file.write_all(bytes)?;
            // Reconstructible caches need atomic visibility, not disk flushes.
            if name == "state.json" || name == METRICS_FILE || name.ends_with(".launch.json") {
                file.sync_all()?;
            }
            fs::rename(self.path.join(&next), self.path.join(name))
        })();
        if result.is_err() {
            let _ = fs::remove_file(self.path.join(next));
        }
        result.map_err(Into::into)
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

pub(crate) fn random_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn validate_state_directory(path: &Path) -> Result<PathBuf> {
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
        serde_json::from_reader(BufReader::new(file.take(STATE_LIMIT))).map_err(Into::into)
    })();
    match result {
        Ok(metrics) => metrics,
        Err(error) => {
            eprintln!("locked-build: ignoring invalid metrics sidecar: {error}");
            crate::metrics::MetricsState::default()
        }
    }
}
