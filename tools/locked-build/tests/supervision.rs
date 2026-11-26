// SPDX-License-Identifier: MIT
//! Real subprocess evidence for exit propagation, leases and safe recovery.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Harness {
    root: TempDir,
    state: PathBuf,
    config: PathBuf,
}

impl Harness {
    fn new(capacity: u32, hold_ms: u64) -> Result<Self, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("state");
        let config = root.path().join("policy.json");
        let sample = root.path().join("sample.json");
        let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
        fs::write(
            &sample,
            serde_json::to_vec(&json!({
                "host": "fixture-host", "observed_at_ms": now,
                "memory_available_bytes": 1000000, "disk_available_bytes": 1000000, "load_one": 0.0,
            }))?,
        )?;
        fs::write(
            &config,
            serde_json::to_vec(&json!({
                "host": "fixture-host", "capacity_units": capacity, "maximum_units": 4,
                "memory_per_unit_bytes": 100, "memory_reserve_bytes": 100, "disk_reserve_bytes": 100,
                "disk_path": root.path(), "maximum_load": 100.0, "maximum_priority": 2,
                "aging_ms": 1000, "hold_ms": hold_ms, "termination_grace_ms": 200,
                "sample_max_age_ms": 60000, "heartbeat_max_age_ms": 10000, "poll_ms": 10,
                "maximum_requests": 100, "sample_file": sample,
                "memory_admission": "live_available", "memory_budget_bytes": null,
                "memory_cap_bytes": null, "memory_cap_by_class": {},
                "on_missing_cgroup": "rss_watchdog",
            }))?,
        )?;
        Ok(Self {
            root,
            state,
            config,
        })
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_locked-build"));
        command
            .arg("--state")
            .arg(&self.state)
            .arg("--config")
            .arg(&self.config);
        command
    }

    fn command(
        &self,
        mode: &str,
        marker: &Path,
        release: &Path,
        units: u32,
    ) -> Result<Child, Box<dyn std::error::Error>> {
        Ok(self
            .cli()
            .arg("run")
            .arg("--owner")
            .arg("test:team:lead")
            .arg("--ticket")
            .arg("TEST-1")
            .arg("--units")
            .arg(units.to_string())
            .arg("--")
            .arg(std::env::current_exe()?)
            .arg("--exact")
            .arg("subprocess_fixture")
            .arg("--ignored")
            .arg("--nocapture")
            .arg("--skip")
            .arg(marker)
            .env("LOCKED_BUILD_FIXTURE_MODE", mode)
            .env("LOCKED_BUILD_FIXTURE_MARKER", marker)
            .env("LOCKED_BUILD_FIXTURE_RELEASE", release)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?)
    }

    fn output(&self, action: &str) -> Result<Output, Box<dyn std::error::Error>> {
        Ok(self.cli().arg(action).output()?)
    }

    fn report(&self) -> Result<Value, Box<dyn std::error::Error>> {
        let output = self.output("status")?;
        assert!(
            output.status.success(),
            "status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn request(&self, index: usize) -> Result<Value, Box<dyn std::error::Error>> {
        self.report()?
            .get("requests")
            .and_then(|requests| requests.get(index))
            .cloned()
            .ok_or_else(|| "request not observed".into())
    }

    fn persisted(&self) -> Result<Value, Box<dyn std::error::Error>> {
        Ok(serde_json::from_slice(&fs::read(
            self.state.join("state.json"),
        )?)?)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let mut pids = std::collections::BTreeSet::new();
        if let Ok(bytes) = fs::read(self.state.join("state.json"))
            && let Ok(state) = serde_json::from_slice::<Value>(&bytes)
        {
            for request in state["requests"].as_array().into_iter().flatten() {
                for role in ["worker", "watchdog"] {
                    if let Some(pid) = request[role]["pid"]
                        .as_u64()
                        .and_then(|pid| u32::try_from(pid).ok())
                    {
                        pids.insert(pid);
                    }
                }
            }
        }
        if let Ok(files) = fs::read_dir(self.root.path()) {
            for file in files.flatten() {
                if file
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "pid")
                    && let Ok(value) = fs::read_to_string(file.path())
                    && let Ok(pid) = value.parse::<u32>()
                {
                    pids.insert(pid);
                }
            }
        }
        let system = sysinfo::System::new_all();
        for pid in pids {
            if let Some(process) = system.process(sysinfo::Pid::from_u32(pid))
                && process
                    .cmd()
                    .iter()
                    .any(|argument| Path::new(argument).starts_with(self.root.path()))
                && let Ok(pid) = i32::try_from(pid)
            {
                let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            }
        }
    }
}

fn observed(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "observable condition did not occur"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn complete(child: &mut Child) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
    complete_before(child, Duration::from_secs(30))
}

fn complete_before(
    child: &mut Child,
    limit: Duration,
) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        assert!(
            Instant::now() < deadline,
            "command survived the cancellation/hold cleanup deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fixture_alive(marker: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let pid: u32 = fs::read_to_string(pid_file(marker))?.parse()?;
    Ok(process_alive(pid))
}

fn pid_file(marker: &Path) -> PathBuf {
    let mut path = marker.as_os_str().to_os_string();
    path.push(".pid");
    PathBuf::from(path)
}

fn process_alive(pid: u32) -> bool {
    let system = sysinfo::System::new_all();
    system
        .process(sysinfo::Pid::from_u32(pid))
        .is_some_and(|process| process.status() != sysinfo::ProcessStatus::Zombie)
}

/// Trace: FR-005-AC-7, FR-005-AC-5
#[test]
fn exit_75_is_executed_once_and_telemetry_excludes_command_arguments() -> TestResult {
    let harness = Harness::new(1, 10000)?;
    let marker = harness.root.path().join("invocations");
    let release = harness.root.path().join("release");
    let mut child = harness.command("exit75", &marker, &release, 1)?;
    assert_eq!(complete(&mut child)?.code(), Some(75));
    assert_eq!(fs::read_to_string(&marker)?, "invoked\n");
    let report = harness.report()?;
    assert_eq!(report["requests"][0]["state"], "failed");
    assert_eq!(report["requests"][0]["exit_code"], 75);
    assert_eq!(report["events"].as_array().map(Vec::len), Some(3));
    assert_eq!(report["used_units"], 0);
    assert!(!serde_json::to_string(&report)?.contains("subprocess_fixture"));
    Ok(())
}

/// Trace: FR-005-AC-4, FR-005-AC-7
#[test]
fn exit_75_survives_forced_descendant_cleanup_and_reaps_its_watchdog() -> TestResult {
    let harness = Harness::new(1, 10000)?;
    let marker = harness.root.path().join("invocations");
    let descendant = marker.with_extension("descendant");
    let mut child = harness.command(
        "exit75-descendant",
        &marker,
        &harness.root.path().join("never-release"),
        1,
    )?;
    assert_eq!(
        complete_before(&mut child, Duration::from_secs(8))?.code(),
        Some(75)
    );
    assert_eq!(fs::read_to_string(&marker)?, "invoked\n");
    assert_eq!(harness.report()?["used_units"], 0);
    assert!(!fixture_alive(&descendant)?);
    Ok(())
}

/// Trace: FR-005-AC-7
#[test]
fn a_signaled_command_returns_non_success() -> TestResult {
    let harness = Harness::new(1, 10000)?;
    let mut child = harness.command(
        "signal",
        &harness.root.path().join("signal"),
        &harness.root.path().join("release"),
        1,
    )?;
    assert_eq!(complete(&mut child)?.code(), Some(143));
    assert_eq!(harness.request(0)?["signal"], 15);
    assert_eq!(harness.report()?["used_units"], 0);
    Ok(())
}

/// Trace: FR-005-AC-1, FR-005-AC-2, FR-005-AC-5
#[test]
fn concurrent_clients_never_duplicate_a_lease_and_run_in_fifo_order() -> TestResult {
    let harness = Harness::new(1, 20000)?;
    let first_marker = harness.root.path().join("first");
    let second_marker = harness.root.path().join("second");
    let first_release = harness.root.path().join("release-first");
    let second_release = harness.root.path().join("release-second");
    let mut first = harness.command("block", &first_marker, &first_release, 1)?;
    observed(|| first_marker.exists());
    let mut second = harness.command("block", &second_marker, &second_release, 1)?;
    observed(|| {
        harness
            .request(1)
            .is_ok_and(|request| request["state"] == "queued")
    });
    assert_eq!(harness.report()?["used_units"], 1);
    assert!(!second_marker.exists());
    fs::write(&first_release, "release")?;
    assert!(complete(&mut first)?.success());
    observed(|| second_marker.exists());
    assert_eq!(harness.report()?["used_units"], 1);
    fs::write(&second_release, "release")?;
    assert!(complete(&mut second)?.success());
    let report = harness.report()?;
    let starts: Vec<_> = report["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|event| event["state"] == "running")
        .map(|event| event["request_id"].clone())
        .collect();
    assert_eq!(
        starts,
        vec![
            report["requests"][0]["request_id"].clone(),
            report["requests"][1]["request_id"].clone()
        ]
    );
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn queued_cancel_never_runs_the_command() -> TestResult {
    let harness = Harness::new(0, 20000)?;
    let marker = harness.root.path().join("must-not-run");
    let mut child = harness.command("block", &marker, &harness.root.path().join("release"), 1)?;
    observed(|| {
        harness
            .request(0)
            .is_ok_and(|request| request["state"] == "queued")
    });
    let request = harness.request(0)?;
    let id = request["request_id"].as_str().ok_or("ID missing")?;
    assert!(harness.cli().arg("cancel").arg(id).status()?.success());
    assert_eq!(complete(&mut child)?.code(), Some(130));
    assert!(!marker.exists());
    assert_eq!(harness.report()?["used_units"], 0);
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn hold_timeout_kills_the_group_before_releasing_capacity() -> TestResult {
    let harness = Harness::new(1, 2000)?;
    let marker = harness.root.path().join("started");
    let mut child = harness.command(
        "ignore-term",
        &marker,
        &harness.root.path().join("never-release"),
        1,
    )?;
    observed(|| marker.exists());
    let descendant = marker.with_extension("descendant");
    observed(|| descendant.exists());
    assert_eq!(harness.report()?["used_units"], 1);
    assert_eq!(complete(&mut child)?.code(), Some(124));
    assert_eq!(harness.request(0)?["state"], "timed_out");
    assert_eq!(harness.report()?["used_units"], 0);
    assert!(!fixture_alive(&marker)?);
    assert!(!fixture_alive(&descendant)?);
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn supervisor_crash_keeps_lease_until_its_child_group_is_verified_stopped() -> TestResult {
    let harness = Harness::new(1, 20000)?;
    let marker = harness.root.path().join("started");
    let release = harness.root.path().join("release");
    let mut child = harness.command("block", &marker, &release, 1)?;
    observed(|| marker.exists());
    let state = harness.persisted()?;
    let pid = state["requests"][0]["worker"]["pid"]
        .as_u64()
        .ok_or("PID missing")?;
    kill(Pid::from_raw(i32::try_from(pid)?), Signal::SIGKILL)?;
    observed(|| {
        harness
            .request(0)
            .is_ok_and(|request| request["suspect"] == true)
    });
    assert_eq!(harness.report()?["used_units"], 1);
    fs::write(&release, "release")?;
    assert_eq!(complete(&mut child)?.code(), Some(1));
    assert_eq!(harness.report()?["used_units"], 0);
    assert_eq!(fs::read_to_string(&marker)?, "invoked\n");
    Ok(())
}

/// Trace: FR-005-AC-3
#[test]
fn invalid_policy_and_oversized_request_are_rejected_without_execution() -> TestResult {
    let harness = Harness::new(1, 10000)?;
    let marker = harness.root.path().join("must-not-run");
    let mut child = harness.command("exit75", &marker, &harness.root.path().join("release"), 5)?;
    assert_eq!(complete(&mut child)?.code(), Some(2));
    assert!(!marker.exists());
    let mut policy: Value = serde_json::from_slice(&fs::read(&harness.config)?)?;
    policy["unexpected_field"] = json!(true);
    fs::write(&harness.config, serde_json::to_vec(&policy)?)?;
    assert!(!harness.output("status")?.status.success());
    Ok(())
}

/// Trace: FR-005-AC-2, FR-005-AC-3
#[test]
fn live_capacity_shrink_reports_overcommit_and_preserves_running_command() -> TestResult {
    let harness = Harness::new(2, 20000)?;
    let marker = harness.root.path().join("started");
    let release = harness.root.path().join("release");
    let mut child = harness.command("block", &marker, &release, 2)?;
    observed(|| marker.exists());
    let mut policy: Value = serde_json::from_slice(&fs::read(&harness.config)?)?;
    policy["capacity_units"] = json!(1);
    let replacement = harness.root.path().join("policy.next");
    fs::write(&replacement, serde_json::to_vec(&policy)?)?;
    fs::rename(replacement, &harness.config)?;
    let report = harness.report()?;
    assert_eq!(report["used_units"], 2);
    assert_eq!(report["free_units"], 0);
    assert_eq!(report["overcommit_units"], 1);
    assert!(child.try_wait()?.is_none());
    fs::write(release, "release")?;
    assert!(complete(&mut child)?.success());
    assert_eq!(harness.report()?["used_units"], 0);
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn dead_queued_supervisor_is_restarted_without_duplicate_command_execution() -> TestResult {
    let harness = Harness::new(0, 20000)?;
    let marker = harness.root.path().join("started");
    let release = harness.root.path().join("release");
    let mut child = harness.command("block", &marker, &release, 1)?;
    observed(|| {
        harness
            .persisted()
            .is_ok_and(|state| state["requests"][0]["worker"]["pid"].is_u64())
    });
    let original = harness.persisted()?["requests"][0]["worker"]["pid"]
        .as_u64()
        .ok_or("PID missing")?;
    kill(Pid::from_raw(i32::try_from(original)?), Signal::SIGKILL)?;
    observed(|| {
        harness.persisted().is_ok_and(|state| {
            state["requests"][0]["worker"]["pid"]
                .as_u64()
                .is_some_and(|pid| pid != original)
        })
    });
    assert!(!marker.exists());
    let mut policy: Value = serde_json::from_slice(&fs::read(&harness.config)?)?;
    policy["capacity_units"] = json!(1);
    let replacement = harness.root.path().join("policy.next");
    fs::write(&replacement, serde_json::to_vec(&policy)?)?;
    fs::rename(replacement, &harness.config)?;
    observed(|| marker.exists());
    fs::write(release, "release")?;
    assert!(complete(&mut child)?.success());
    assert_eq!(fs::read_to_string(marker)?, "invoked\n");
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn policy_read_failure_stops_owned_work_and_retains_lease_until_recovery() -> TestResult {
    let harness = Harness::new(1, 20000)?;
    let marker = harness.root.path().join("started");
    let mut child = harness.command("block", &marker, &harness.root.path().join("release"), 1)?;
    observed(|| marker.exists());
    let worker = u32::try_from(
        harness.persisted()?["requests"][0]["worker"]["pid"]
            .as_u64()
            .ok_or("PID missing")?,
    )?;
    let original = fs::read(&harness.config)?;
    let replacement = harness.root.path().join("policy.next");
    fs::write(&replacement, "invalid json")?;
    fs::rename(&replacement, &harness.config)?;
    assert_eq!(complete(&mut child)?.code(), Some(2));
    observed(|| fixture_alive(&marker).is_ok_and(|alive| !alive) && !process_alive(worker));
    fs::write(&replacement, original)?;
    fs::rename(&replacement, &harness.config)?;
    observed(|| {
        harness
            .output("recover")
            .is_ok_and(|output| output.status.success())
            && harness
                .report()
                .is_ok_and(|report| report["used_units"] == 0)
    });
    assert_eq!(harness.request(0)?["state"], "failed");
    Ok(())
}

/// Trace: FR-005-AC-3, FR-005-AC-5
#[test]
fn relative_host_sample_paths_work_from_another_directory_and_prune_removes_logs() -> TestResult {
    let harness = Harness::new(1, 10000)?;
    let mut policy: Value = serde_json::from_slice(&fs::read(&harness.config)?)?;
    policy["sample_file"] = json!("sample.json");
    policy["disk_path"] = json!(".");
    fs::write(&harness.config, serde_json::to_vec(&policy)?)?;
    let marker = harness.root.path().join("invoked");
    let mut child = harness.command("exit75", &marker, &harness.root.path().join("release"), 1)?;
    assert_eq!(complete(&mut child)?.code(), Some(75));
    let request = harness.request(0)?;
    let id = request["request_id"].as_str().ok_or("ID missing")?;
    let log = harness.state.join(format!("{id}.stdout"));
    assert!(log.exists());
    assert!(harness.output("prune")?.status.success());
    assert!(!log.exists());
    assert_eq!(harness.report()?["requests"], json!([]));
    assert_eq!(harness.report()?["pruned_events"], 3);
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn running_cancel_returns_cancelled_after_group_cleanup() -> TestResult {
    let harness = Harness::new(1, 20000)?;
    let marker = harness.root.path().join("started");
    let mut child = harness.command("block", &marker, &harness.root.path().join("release"), 1)?;
    observed(|| marker.exists());
    let request = harness.request(0)?;
    let id = request["request_id"].as_str().ok_or("ID missing")?;
    assert!(harness.cli().arg("cancel").arg(id).status()?.success());
    assert_eq!(complete(&mut child)?.code(), Some(130));
    assert_eq!(harness.request(0)?["state"], "cancelled");
    assert_eq!(harness.request(0)?["reason"], "cancelled");
    assert_eq!(harness.report()?["used_units"], 0);
    assert!(!fixture_alive(&marker)?);
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn cancellation_after_supervisor_loss_stops_owned_descendants_and_preserves_unrelated_work()
-> TestResult {
    let harness = Harness::new(1, 20000)?;
    let marker = harness.root.path().join("owned");
    let descendant = marker.with_extension("descendant");
    let mut child = harness.command(
        "ignore-term",
        &marker,
        &harness.root.path().join("never-release"),
        1,
    )?;
    observed(|| marker.exists() && descendant.exists());
    let unrelated = harness.root.path().join("unrelated");
    let unrelated_release = harness.root.path().join("unrelated-release");
    let mut other = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("subprocess_fixture")
        .arg("--ignored")
        .arg("--nocapture")
        .arg("--skip")
        .arg(&unrelated)
        .env("LOCKED_BUILD_FIXTURE_MODE", "block")
        .env("LOCKED_BUILD_FIXTURE_MARKER", &unrelated)
        .env("LOCKED_BUILD_FIXTURE_RELEASE", &unrelated_release)
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    observed(|| unrelated.exists());
    let request = harness.request(0)?;
    let id = request["request_id"].as_str().ok_or("ID missing")?;
    let pid = harness.persisted()?["requests"][0]["worker"]["pid"]
        .as_u64()
        .ok_or("PID missing")?;
    kill(Pid::from_raw(i32::try_from(pid)?), Signal::SIGKILL)?;
    assert!(harness.cli().arg("cancel").arg(id).status()?.success());
    assert_eq!(
        complete_before(&mut child, Duration::from_secs(8))?.code(),
        Some(130)
    );
    assert_eq!(harness.report()?["used_units"], 0);
    assert!(!fixture_alive(&marker)?);
    assert!(!fixture_alive(&descendant)?);
    assert!(fixture_alive(&unrelated)?);
    assert!(
        other.try_wait()?.is_none(),
        "watchdog cleanup affected an unrelated process group"
    );
    fs::write(unrelated_release, "release")?;
    assert!(complete(&mut other)?.success());
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn hold_deadline_survives_supervisor_loss_and_stops_term_resistant_descendants() -> TestResult {
    let harness = Harness::new(1, 2000)?;
    let marker = harness.root.path().join("owned");
    let descendant = marker.with_extension("descendant");
    let mut child = harness.command(
        "ignore-term",
        &marker,
        &harness.root.path().join("never-release"),
        1,
    )?;
    observed(|| marker.exists() && descendant.exists());
    let pid = harness.persisted()?["requests"][0]["worker"]["pid"]
        .as_u64()
        .ok_or("PID missing")?;
    kill(Pid::from_raw(i32::try_from(pid)?), Signal::SIGKILL)?;
    assert_eq!(
        complete_before(&mut child, Duration::from_secs(8))?.code(),
        Some(124)
    );
    assert_eq!(harness.request(0)?["state"], "timed_out");
    assert_eq!(harness.request(0)?["reason"], "hold_timeout");
    assert!(!fixture_alive(&marker)?);
    assert!(!fixture_alive(&descendant)?);
    assert_eq!(harness.report()?["used_units"], 0);
    Ok(())
}

#[test]
#[ignore = "subprocess workload, invoked by supervision tests"]
fn subprocess_fixture() -> TestResult {
    let mode = std::env::var("LOCKED_BUILD_FIXTURE_MODE")?;
    let marker =
        PathBuf::from(std::env::var_os("LOCKED_BUILD_FIXTURE_MARKER").ok_or("marker missing")?);
    let release =
        PathBuf::from(std::env::var_os("LOCKED_BUILD_FIXTURE_RELEASE").ok_or("release missing")?);
    if mode == "ignore-term" || mode == "leaf-ignore-term" {
        signal_hook::flag::register(
            signal_hook::consts::SIGTERM,
            Arc::new(AtomicBool::new(false)),
        )?;
    }
    if mode == "ignore-term" || mode == "exit75-descendant" {
        let descendant = marker.with_extension("descendant");
        Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("subprocess_fixture")
            .arg("--ignored")
            .arg("--nocapture")
            .arg("--skip")
            .arg(&descendant)
            .env("LOCKED_BUILD_FIXTURE_MODE", "leaf-ignore-term")
            .env("LOCKED_BUILD_FIXTURE_MARKER", &descendant)
            .spawn()?;
        observed(|| descendant.exists());
    }
    fs::write(pid_file(&marker), std::process::id().to_string())?;
    let mut file = OpenOptions::new().create(true).append(true).open(marker)?;
    writeln!(file, "invoked")?;
    file.sync_all()?;
    match mode.as_str() {
        "exit75" | "exit75-descendant" => std::process::exit(75),
        "signal" => {
            kill(Pid::this(), Signal::SIGTERM)?;
        }
        "block" | "ignore-term" | "leaf-ignore-term" => {
            while !release.exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        _ => return Err("unknown fixture mode".into()),
    }
    Ok(())
}
