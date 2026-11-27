// SPDX-License-Identifier: MIT
//! Validated host policy and observed resources.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

use crate::error::{Error, ErrorCode, Result};

pub(crate) const MAX_REQUESTS: usize = 100_000;
pub(crate) const SAMPLE_MAX_AGE_MS: u64 = 5_000;
pub(crate) const HEARTBEAT_MAX_AGE_MS: u64 = 10_000;
pub(crate) const POLL_INTERVAL_MS: u64 = 250;

/// Policy for admitting build leases against host memory.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryAdmissionMode {
    /// Compare live available memory with per-unit leases and reserve.
    LiveAvailable,
    /// Charge each active lease's fixed memory cap against a configured budget.
    #[default]
    LeaseAccounted,
}

/// Action to take when a Linux user systemd scope cannot be started.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OnMissingCgroup {
    /// Fail the lease without starting its command.
    Refuse,
    /// Enforce the configured cap with the process-group RSS watchdog.
    #[default]
    RssWatchdog,
}

/// Admission policy. Every capacity and limit is explicitly configured.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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
    /// Priority aging step; reservation starts after (P+1) steps.
    pub aging_ms: u64,
    /// Maximum wall duration of any running command.
    pub hold_ms: u64,
    /// Grace after SIGTERM before SIGKILL.
    pub termination_grace_ms: u64,
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
    /// Delete a target directory when its measured size exceeds this limit.
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// Named priority classes and their numeric scheduler priorities.
    pub priority_classes: BTreeMap<String, u32>,
    /// Real Cargo executable used by the optional PATH shim.
    #[serde(default)]
    pub real_cargo: Option<PathBuf>,
    /// Selects live available-memory admission or fixed lease accounting.
    #[serde(default)]
    pub memory_admission: MemoryAdmissionMode,
    /// Total build memory budget for `lease_accounted` admission.
    pub memory_budget_bytes: Option<u64>,
    /// Estimated memory used by one Cargo job or resource unit.
    pub memory_per_job_bytes: u64,
    /// Per-class per-job memory estimates.
    #[serde(default)]
    pub memory_per_job_by_class: BTreeMap<String, u64>,
    /// Maximum lease size accepted for each class.
    #[serde(default)]
    pub memory_cap_by_class: BTreeMap<String, u64>,
    /// Action when a Linux systemd scope is unavailable.
    #[serde(default)]
    pub on_missing_cgroup: OnMissingCgroup,
}

impl Default for Config {
    fn default() -> Self {
        let mut system = System::new_all();
        system.refresh_memory();
        let total_memory = system.total_memory();
        let memory_reserve_bytes = total_memory / 4;
        let memory_budget_bytes = memory_budget_after_reserve(total_memory, memory_reserve_bytes);
        let priority_classes = BTreeMap::from([
            ("must-fix".to_owned(), 3),
            ("point".to_owned(), 2),
            ("block".to_owned(), 1),
            ("periodic".to_owned(), 0),
            ("arch-test".to_owned(), 2),
            ("kani".to_owned(), 0),
        ]);
        Self {
            host: "localhost".to_owned(),
            capacity_units: 1,
            maximum_units: 1,
            memory_per_unit_bytes: 0,
            memory_reserve_bytes,
            disk_reserve_bytes: 0,
            disk_path: PathBuf::from("."),
            maximum_load: 8.0,
            aging_ms: 60_000,
            hold_ms: 5_400_000,
            termination_grace_ms: 10_000,
            sample_file: None,
            cargo_build_jobs: None,
            rust_test_threads: None,
            cargo_incremental: None,
            target_root: None,
            max_bytes: None,
            priority_classes,
            real_cargo: None,
            memory_admission: MemoryAdmissionMode::LeaseAccounted,
            memory_budget_bytes,
            memory_per_job_bytes: 2_500_000_000,
            memory_per_job_by_class: BTreeMap::new(),
            memory_cap_by_class: BTreeMap::new(),
            on_missing_cgroup: OnMissingCgroup::RssWatchdog,
        }
    }
}

fn memory_budget_after_reserve(total_memory: u64, memory_reserve: u64) -> Option<u64> {
    (total_memory > 0 && memory_reserve < total_memory).then_some(total_memory - memory_reserve)
}

impl Config {
    #[allow(clippy::field_reassign_with_default)]
    pub(crate) fn measured_default() -> Result<Self> {
        let mut config = Self::default();
        config.host = System::host_name().unwrap_or_else(|| "localhost".to_owned());
        let cores = std::thread::available_parallelism()
            .map_err(|error| Error::new(ErrorCode::Configuration, error.to_string()))?;
        let cores = u32::try_from(cores.get()).map_err(|_| {
            Error::new(
                ErrorCode::Configuration,
                "host core count exceeds policy range",
            )
        })?;
        let cores = cores.max(1);
        config.capacity_units = cores;
        config.maximum_units = cores;
        config.maximum_load = f64::from(cores) * 2.0;
        let memory_budget = config.memory_budget_bytes.ok_or_else(|| {
            Error::new(
                ErrorCode::Configuration,
                "cannot measure host memory for `memory_budget_bytes`",
            )
        })?;
        let safe_jobs = (memory_budget / config.memory_per_job_bytes).max(1);
        config.cargo_build_jobs = Some(cores.min(u32::try_from(safe_jobs).unwrap_or(u32::MAX)));

        let disk_path = std::env::current_dir()?.canonicalize()?;
        let disks = Disks::new_with_refreshed_list();
        let disk = disks
            .list()
            .iter()
            .filter(|disk| disk_path.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().components().count())
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::Configuration,
                    "cannot measure disk free space for `disk_reserve_bytes`",
                )
            })?;
        config.disk_path = disk_path;
        config.disk_reserve_bytes = disk.available_space() / 10;
        config.validate()?;
        Ok(config)
    }

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
        let invalid = |field: &str, detail: &str| {
            Error::new(
                ErrorCode::Configuration,
                format!("invalid policy field `{field}`: {detail}"),
            )
        };
        if self.host.is_empty() || self.host.len() > 128 {
            return Err(invalid("host", "must contain 1 to 128 characters"));
        }
        if self.maximum_units == 0 || self.capacity_units > self.maximum_units {
            return Err(invalid(
                "maximum_units",
                "must be positive and at least capacity_units",
            ));
        }
        if !self.maximum_load.is_finite() || self.maximum_load <= 0.0 {
            return Err(invalid("maximum_load", "must be finite and positive"));
        }
        if self.aging_ms == 0
            || (u64::from(self.max_priority()) + 1)
                .checked_mul(self.aging_ms)
                .is_none()
        {
            return Err(invalid(
                "aging_ms",
                "must be positive and fit the priority range",
            ));
        }
        if self.hold_ms == 0 {
            return Err(invalid("hold_ms", "must be positive"));
        }
        if self.termination_grace_ms == 0
            || std::time::Instant::now()
                .checked_add(std::time::Duration::from_millis(self.termination_grace_ms))
                .is_none()
        {
            return Err(invalid(
                "termination_grace_ms",
                "must be positive and representable",
            ));
        }
        if self.cargo_build_jobs.is_some_and(|jobs| jobs == 0) {
            return Err(invalid("cargo_build_jobs", "must be positive when set"));
        }
        if self.rust_test_threads.is_some_and(|threads| threads == 0) {
            return Err(invalid("rust_test_threads", "must be positive when set"));
        }
        if self.max_bytes.is_some() && self.target_root.is_none() {
            return Err(invalid("target_root", "is required when max_bytes is set"));
        }
        if self.priority_classes.iter().any(|(name, _)| {
            !matches!(
                name.as_str(),
                "must-fix" | "point" | "block" | "periodic" | "arch-test" | "kani"
            )
        }) {
            return Err(invalid("priority_classes", "contains an unknown class"));
        }
        if self.memory_per_job_bytes == 0 {
            return Err(invalid("memory_per_job_bytes", "must be positive"));
        }
        if self
            .memory_per_job_by_class
            .iter()
            .any(|(class, estimate)| {
                !matches!(
                    class.as_str(),
                    "must-fix" | "point" | "block" | "periodic" | "arch-test" | "kani"
                ) || *estimate == 0
                    || self
                        .memory_cap_by_class
                        .get(class)
                        .is_some_and(|class_cap| *estimate > *class_cap)
            })
        {
            return Err(invalid(
                "memory_per_job_by_class",
                "estimates must be positive and no larger than the class memory cap",
            ));
        }
        if self.memory_cap_by_class.iter().any(|(name, cap)| {
            !matches!(
                name.as_str(),
                "must-fix" | "point" | "block" | "periodic" | "arch-test" | "kani"
            ) || *cap == 0
        }) {
            return Err(invalid(
                "memory_cap_by_class",
                "contains an unknown class or zero cap",
            ));
        }
        match self.memory_admission {
            MemoryAdmissionMode::LiveAvailable => {
                if self.memory_per_unit_bytes == 0 {
                    return Err(invalid(
                        "memory_per_unit_bytes",
                        "must be positive in live_available mode",
                    ));
                }
                if u64::from(self.maximum_units)
                    .checked_mul(self.memory_per_unit_bytes)
                    .is_none()
                {
                    return Err(invalid(
                        "memory_per_unit_bytes",
                        "overflows the maximum unit budget",
                    ));
                }
            }
            MemoryAdmissionMode::LeaseAccounted => {
                let budget = self.memory_budget_bytes.unwrap_or_default();
                if budget == 0 {
                    return Err(invalid(
                        "memory_budget_bytes",
                        "must be positive in lease_accounted mode",
                    ));
                }
                if self.memory_cap_by_class.values().any(|cap| *cap > budget) {
                    return Err(invalid(
                        "memory_cap_by_class",
                        "a cap exceeds memory_budget_bytes",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn max_priority(&self) -> u32 {
        self.priority_classes.values().copied().max().unwrap_or(0)
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
            || now.saturating_sub(self.observed_at_ms) > SAMPLE_MAX_AGE_MS;
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

    #[test]
    fn default_policy_hold_limit_is_ninety_minutes() {
        assert_eq!(Config::default().hold_ms, 5_400_000);
    }

    fn config() -> Result<Config> {
        let mut config: Config = serde_json::from_str(include_str!("../policy.example.json"))?;
        config.memory_admission = MemoryAdmissionMode::LiveAvailable;
        Ok(config)
    }

    #[test]
    fn missing_memory_admission_field_defaults_to_lease_accounted() -> Result<()> {
        let mut value: serde_json::Value =
            serde_json::from_str(include_str!("../policy.example.json"))?;
        value
            .as_object_mut()
            .ok_or_else(|| Error::new(ErrorCode::Configuration, "policy fixture is not an object"))?
            .remove("memory_admission");
        let config: Config = serde_json::from_value(value)?;

        assert_eq!(config.memory_admission, MemoryAdmissionMode::LeaseAccounted);
        Ok(())
    }

    #[test]
    fn three_field_policy_uses_defaults_and_validates() -> Result<()> {
        let config: Config =
            serde_json::from_str(r#"{"host":"build-host","capacity_units":1,"maximum_units":1}"#)?;
        assert_eq!(config.memory_admission, MemoryAdmissionMode::LeaseAccounted);
        assert_eq!(config.on_missing_cgroup, OnMissingCgroup::RssWatchdog);
        assert_eq!(config.priority_classes.get("periodic"), Some(&0));
        config.validate()
    }

    #[test]
    fn default_memory_budget_subtracts_the_reserve_from_measured_ram() {
        const GIB: u64 = 1_000_000_000;
        assert_eq!(
            memory_budget_after_reserve(62 * GIB, 8 * GIB),
            Some(54 * GIB)
        );
        assert_eq!(memory_budget_after_reserve(8 * GIB, 8 * GIB), None);
    }

    #[test]
    fn example_policy_selects_lease_accounted_with_a_valid_cap_budget() -> Result<()> {
        let config: Config = serde_json::from_str(include_str!("../policy.example.json"))?;

        assert_eq!(config.memory_admission, MemoryAdmissionMode::LeaseAccounted);
        assert_eq!(config.memory_budget_bytes, Some(17_179_869_184));
        assert_eq!(config.memory_per_job_bytes, 2_500_000_000);
        config.validate()
    }

    #[test]
    fn lease_accounted_does_not_require_per_unit_or_reserve_values() -> Result<()> {
        let mut value: serde_json::Value =
            serde_json::from_str(include_str!("../policy.example.json"))?;
        let object = value.as_object_mut().ok_or_else(|| {
            Error::new(ErrorCode::Configuration, "policy fixture is not an object")
        })?;
        object.remove("memory_per_unit_bytes");
        object.remove("memory_reserve_bytes");
        object.remove("memory_budget_bytes");
        let config: Config = serde_json::from_value(value)?;
        assert_eq!(config.memory_per_unit_bytes, 0);
        assert!(config.memory_reserve_bytes > 0);
        assert!(config.memory_budget_bytes.is_some_and(|budget| budget > 0));
        config.validate()
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
