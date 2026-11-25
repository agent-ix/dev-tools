// SPDX-License-Identifier: MIT
//! Validated host policy and observed resources.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

use crate::error::{Error, ErrorCode, Result};

/// Admission policy. Every capacity and limit is explicitly configured.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Stable host label attached to telemetry.
    pub host: String,
    /// Currently enabled resource units; may shrink without killing work.
    pub capacity_units: u32,
    /// Maximum feasible request size, independent of temporary shrink.
    pub maximum_units: u32,
    /// Memory budget charged for each resource unit.
    pub memory_per_unit_bytes: u64,
    /// Free memory kept outside build leases.
    pub memory_reserve_bytes: u64,
    /// Filesystem space kept outside build leases.
    pub disk_reserve_bytes: u64,
    /// Directory whose filesystem is measured.
    pub disk_path: PathBuf,
    /// Maximum observed one-minute host load for new admission.
    pub maximum_load: f64,
    /// Largest accepted base priority.
    pub maximum_priority: u32,
    /// Priority aging step; reservation starts after (P+1) steps.
    pub aging_ms: u64,
    /// Maximum wall duration of any running command.
    pub hold_ms: u64,
    /// Grace after SIGTERM before SIGKILL.
    pub termination_grace_ms: u64,
    /// Maximum accepted resource sample age.
    pub sample_max_age_ms: u64,
    /// Supervisor heartbeat age after which a lease is reported suspect.
    pub heartbeat_max_age_ms: u64,
    /// Polling cadence, not a fairness or admission limit.
    pub poll_ms: u64,
    /// Bounded retained requests; terminal requests can be pruned explicitly.
    pub maximum_requests: usize,
    /// Optional host-agent JSON measurement; absent uses local live telemetry.
    pub sample_file: Option<PathBuf>,
}

impl Config {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_reader(std::fs::File::open(path)?)
            .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
        Self::from_value(path, value)
    }

    pub(crate) fn read_for_observe(path: &Path) -> Result<ObserveConfig> {
        let mut value: serde_json::Value = serde_json::from_reader(std::fs::File::open(path)?)
            .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
        let capacity_units = optional_limit(&value, "capacity_units")?;
        let maximum_units = optional_limit(&value, "maximum_units")?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| Error::new(ErrorCode::Configuration, "host policy must be an object"))?;
        // Config::validate checks the entire existing policy schema. These local
        // placeholders satisfy only its required numeric fields when absent; they
        // are never returned to the observer or used for admission or exposition.
        if capacity_units.is_none() {
            object.insert("capacity_units".into(), serde_json::json!(0));
        }
        if maximum_units.is_none() {
            object.insert(
                "maximum_units".into(),
                serde_json::json!(capacity_units.unwrap_or(0).max(1)),
            );
        }
        let config = Self::from_value(path, value)?;
        Ok(ObserveConfig {
            host: config.host,
            capacity_units,
        })
    }

    fn from_value(path: &Path, value: serde_json::Value) -> Result<Self> {
        let mut config: Self = serde_json::from_value(value)
            .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
        let directory = path
            .parent()
            .ok_or_else(|| Error::new(ErrorCode::Configuration, "policy path has no directory"))?;
        if config.disk_path.is_relative() {
            config.disk_path = directory.join(&config.disk_path);
        }
        if let Some(sample) = &mut config.sample_file
            && sample.is_relative()
        {
            *sample = directory.join(&*sample);
        }
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let valid = !self.host.is_empty()
            && self.host.len() <= 128
            && self.capacity_units <= self.maximum_units
            && self.maximum_units > 0
            && self.memory_per_unit_bytes > 0
            && self.maximum_load.is_finite()
            && self.maximum_load > 0.0
            && self.aging_ms > 0
            && self.hold_ms > 0
            && self.termination_grace_ms > 0
            && self.sample_max_age_ms > 0
            && self.heartbeat_max_age_ms > self.poll_ms
            && self.poll_ms > 0
            && self.poll_ms <= self.hold_ms
            && self.poll_ms <= self.termination_grace_ms
            && std::time::Instant::now()
                .checked_add(std::time::Duration::from_millis(self.termination_grace_ms))
                .is_some()
            && self.maximum_requests > 0
            && self.maximum_requests <= 100_000
            && u64::from(self.maximum_units)
                .checked_mul(self.memory_per_unit_bytes)
                .is_some()
            && (u64::from(self.maximum_priority) + 1)
                .checked_mul(self.aging_ms)
                .is_some();
        if !valid {
            return Err(Error::new(
                ErrorCode::Configuration,
                "invalid host policy or limit",
            ));
        }
        Ok(())
    }

    pub(crate) fn sample(&self, now: u64) -> Result<HostSample> {
        if let Some(path) = &self.sample_file {
            let sample: HostSample = serde_json::from_reader(std::fs::File::open(path)?)?;
            return Ok(sample);
        }
        let system = System::new_all();
        let path = self.disk_path.canonicalize()?;
        let disks = Disks::new_with_refreshed_list();
        let disk = disks
            .list()
            .iter()
            .filter(|disk| path.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().components().count())
            .ok_or_else(|| Error::new(ErrorCode::Configuration, "disk telemetry unavailable"))?;
        Ok(HostSample {
            host: self.host.clone(),
            observed_at_ms: now,
            memory_available_bytes: system.available_memory(),
            disk_available_bytes: disk.available_space(),
            load_one: System::load_average().one,
        })
    }
}

pub(crate) struct ObserveConfig {
    pub(crate) host: String,
    pub(crate) capacity_units: Option<u32>,
}

fn optional_limit(value: &serde_json::Value, key: &str) -> Result<Option<u32>> {
    let Some(value) = value.get(key) else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))
}

/// Host-agent sample accepted by the same path as local measurements.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostSample {
    /// Host identity must match the configured host.
    pub host: String,
    /// Unix UTC timestamp in milliseconds.
    pub observed_at_ms: u64,
    /// Available RAM, before the scheduler's conservative lease charge.
    pub memory_available_bytes: u64,
    /// Available bytes on the configured build filesystem.
    pub disk_available_bytes: u64,
    /// Observed one-minute host load.
    pub load_one: f64,
}

impl HostSample {
    pub(crate) fn block_reason(
        &self,
        config: &Config,
        now: u64,
        leased: u32,
        requested: u32,
    ) -> Option<&'static str> {
        if self.host != config.host
            || self.observed_at_ms > now
            || now.saturating_sub(self.observed_at_ms) > config.sample_max_age_ms
        {
            return Some("host_sample_invalid_or_stale");
        }
        if !self.load_one.is_finite() || self.load_one < 0.0 || self.load_one >= config.maximum_load
        {
            return Some("host_load");
        }
        if self.disk_available_bytes < config.disk_reserve_bytes {
            return Some("disk_reserve");
        }
        let needed = u64::from(leased)
            .saturating_add(u64::from(requested))
            .saturating_mul(config.memory_per_unit_bytes)
            .saturating_add(config.memory_reserve_bytes);
        if self.memory_available_bytes < needed {
            return Some("memory_reserve");
        }
        None
    }
}
