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
        state.validate(&config)?;
        let before = serde_json::to_vec(&state)?;
        let output = operation(&mut state, &config)?;
        state.validate(&config)?;
        let bytes = serde_json::to_vec(&state)?;
        if bytes == before && self.path.join("state.json").try_exists()? {
            return Ok(output);
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > STATE_LIMIT {
            return Err(Error::new(
                ErrorCode::State,
                "state size limit reached; prune terminal requests",
            ));
        }
        self.reject_link("state.next")?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(self.path.join("state.next"))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(self.path.join("state.next"), self.path.join("state.json"))?;
        File::open(&self.path)?.sync_all()?;
        Ok(output)
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
