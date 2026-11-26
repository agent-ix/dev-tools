// SPDX-License-Identifier: MIT
//! Validated host policy and observed resources.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

use crate::error::{Error, ErrorCode, Result};

/// Policy for admitting build leases against host memory.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryAdmissionMode {
    /// Compare live available memory with per-unit leases and reserve.
    #[default]
    LiveAvailable,
    /// Charge each active lease's fixed memory cap against a configured budget.
    LeaseAccounted,
}

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
    /// Maximum cargo jobs for an otherwise idle host, divided by active leased units.
    #[serde(default)]
    pub cargo_build_jobs: Option<u32>,
    /// Rust test thread count set for each leased command.
    #[serde(default)]
    pub rust_test_threads: Option<u32>,
    /// Incremental compilation setting inherited by each leased command when configured.
    #[serde(default)]
    pub cargo_incremental: Option<bool>,
    /// Root under which owner-specific Cargo target directories are created.
    #[serde(default)]
    pub target_root: Option<PathBuf>,
    /// Delete an idle target directory after this many milliseconds.
    #[serde(default)]
    pub trim_after_ms: Option<u64>,
    /// Delete a target directory when its measured size exceeds this limit.
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// Named priority classes and their numeric scheduler priorities.
    #[serde(default)]
    pub priority_classes: BTreeMap<String, u32>,
    /// Real Cargo executable used by the optional PATH shim.
    #[serde(default)]
    pub real_cargo: Option<PathBuf>,
    /// Selects live available-memory admission or fixed lease accounting.
    #[serde(default)]
    pub memory_admission: MemoryAdmissionMode,
    /// Total build memory budget for `lease_accounted` admission.
    #[serde(default)]
    pub memory_budget_bytes: Option<u64>,
    /// Default enforced memory cap for a lease, if configured.
    #[serde(default)]
    pub memory_cap_bytes: Option<u64>,
    /// Per-class memory-cap overrides.
    #[serde(default)]
    pub memory_cap_by_class: BTreeMap<String, u64>,
}

impl Config {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let mut config: Self = serde_json::from_reader(std::fs::File::open(path)?)
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
        if let Some(target_root) = &mut config.target_root
            && target_root.is_relative()
        {
            *target_root = directory.join(&*target_root);
        }
        if let Some(real_cargo) = &mut config.real_cargo
            && real_cargo.is_relative()
        {
            *real_cargo = directory.join(&*real_cargo);
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
            && self.cargo_build_jobs.is_none_or(|jobs| jobs > 0)
            && self.rust_test_threads.is_none_or(|threads| threads > 0)
            && (self.trim_after_ms.is_none() && self.max_bytes.is_none()
                || self.target_root.is_some())
            && self.priority_classes.iter().all(|(name, priority)| {
                matches!(name.as_str(), "must-fix" | "point" | "block" | "periodic")
                    && *priority <= self.maximum_priority
            })
            && self.memory_cap_bytes.is_none_or(|cap| cap > 0)
            && self.memory_cap_by_class.iter().all(|(name, cap)| {
                matches!(name.as_str(), "must-fix" | "point" | "block" | "periodic") && *cap > 0
            })
            && (match self.memory_admission {
                MemoryAdmissionMode::LiveAvailable => true,
                MemoryAdmissionMode::LeaseAccounted => {
                    self.memory_budget_bytes.is_some_and(|budget| budget > 0)
                        && self.memory_cap_bytes.is_some_and(|cap| {
                            cap > 0 && self.memory_budget_bytes.is_some_and(|budget| cap <= budget)
                        })
                        && self.memory_cap_by_class.values().all(|cap| {
                            self.memory_budget_bytes
                                .is_some_and(|budget| *cap <= budget)
                        })
                }
            })
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
        let path = self.disk_path.canonicalize()?;
        let disks = Disks::new_with_refreshed_list();
        let disk = disks
            .list()
            .iter()
            .filter(|disk| path.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().components().count())
            .ok_or_else(|| Error::new(ErrorCode::Configuration, "disk telemetry unavailable"))?;
        let memory_available_bytes = match self.memory_admission {
            MemoryAdmissionMode::LiveAvailable => System::new_all().available_memory(),
            MemoryAdmissionMode::LeaseAccounted => u64::MAX,
        };
        Ok(HostSample {
            host: self.host.clone(),
            observed_at_ms: now,
            memory_available_bytes,
            disk_available_bytes: disk.available_space(),
            load_one: System::load_average().one,
        })
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockReason {
    Probe,
    HostLoad,
    TargetDisk,
    Memory,
    Swap,
    Compressor,
    Operator,
}

impl BlockReason {
    const PRECEDENCE: [Self; 7] = [
        Self::Probe,
        Self::HostLoad,
        Self::TargetDisk,
        Self::Memory,
        Self::Swap,
        Self::Compressor,
        Self::Operator,
    ];

    fn select(reasons: &[Self]) -> Option<Self> {
        Self::PRECEDENCE
            .into_iter()
            .find(|candidate| reasons.contains(candidate))
    }

    fn admission_reason(self) -> &'static str {
        match self {
            Self::Probe => "host_sample_invalid_or_stale",
            Self::HostLoad => "host_load",
            Self::TargetDisk => "disk_reserve",
            Self::Memory => "memory_reserve",
            Self::Swap => "swap_reserve",
            Self::Compressor => "compressor_reserve",
            Self::Operator => "operator_hold",
        }
    }
}

impl HostSample {
    pub(crate) fn block_reason(
        &self,
        config: &Config,
        now: u64,
        leased: u32,
        requested: u32,
    ) -> Option<&'static str> {
        let sample_is_invalid = self.host != config.host
            || self.observed_at_ms > now
            || now.saturating_sub(self.observed_at_ms) > config.sample_max_age_ms;
        let mut reasons = Vec::with_capacity(4);
        if sample_is_invalid {
            reasons.push(BlockReason::Probe);
        } else {
            if !self.load_one.is_finite()
                || self.load_one < 0.0
                || self.load_one >= config.maximum_load
            {
                reasons.push(BlockReason::HostLoad);
            }
            if self.disk_available_bytes < config.disk_reserve_bytes {
                reasons.push(BlockReason::TargetDisk);
            }
            if config.memory_admission == MemoryAdmissionMode::LiveAvailable {
                let needed = u64::from(leased)
                    .saturating_add(u64::from(requested))
                    .saturating_mul(config.memory_per_unit_bytes)
                    .saturating_add(config.memory_reserve_bytes);
                if self.memory_available_bytes < needed {
                    reasons.push(BlockReason::Memory);
                }
            }
        }
        BlockReason::select(&reasons).map(BlockReason::admission_reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Result<Config> {
        Ok(serde_json::from_str(include_str!(
            "../policy.example.json"
        ))?)
    }

    /// Trace: FR-015-AC-11
    #[test]
    fn admission_reason_uses_documented_precedence_for_available_probes() -> Result<()> {
        let mut policy = config()?;
        policy.memory_reserve_bytes = 1_000;
        policy.disk_reserve_bytes = 1_000;

        let mut sample = HostSample {
            host: policy.host.clone(),
            observed_at_ms: 100,
            memory_available_bytes: 0,
            disk_available_bytes: 0,
            load_one: f64::MAX,
        };
        sample.host = "different-host".to_owned();
        assert_eq!(
            sample.block_reason(&policy, 100, 0, 1),
            Some("host_sample_invalid_or_stale")
        );

        sample.host.clone_from(&policy.host);
        assert_eq!(sample.block_reason(&policy, 100, 0, 1), Some("host_load"));

        sample.load_one = 0.0;
        assert_eq!(
            sample.block_reason(&policy, 100, 0, 1),
            Some("disk_reserve")
        );

        sample.disk_available_bytes = 1_000;
        assert_eq!(
            sample.block_reason(&policy, 100, 0, 1),
            Some("memory_reserve")
        );
        Ok(())
    }

    /// Trace: FR-015-AC-11
    #[test]
    fn all_seven_block_reasons_have_the_documented_precedence() {
        let candidates = [
            BlockReason::Operator,
            BlockReason::Compressor,
            BlockReason::Swap,
            BlockReason::Memory,
            BlockReason::TargetDisk,
            BlockReason::HostLoad,
            BlockReason::Probe,
        ];
        for (index, expected) in BlockReason::PRECEDENCE.into_iter().enumerate() {
            assert_eq!(
                BlockReason::select(&candidates[..=6 - index]),
                Some(expected)
            );
        }
    }
}
