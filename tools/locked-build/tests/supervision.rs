// SPDX-License-Identifier: MIT
//! Real subprocess evidence for exit propagation, leases and safe recovery.

use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[cfg(target_os = "linux")]
struct NamespaceChild(Child);

#[cfg(target_os = "linux")]
impl Drop for NamespaceChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Provenance: SWM-421, SWM-400. Trace: FR-005-AC-4
#[cfg(target_os = "linux")]
#[test]
#[ignore = "native Linux unprivileged namespace recovery and operator release"]
fn unshare_recovery_drains_only_the_extinct_namespace() -> TestResult {
    let harness = Harness::new(3, 120000)?;
    fs::create_dir_all(&harness.state)?;
    fs::set_permissions(&harness.state, fs::Permissions::from_mode(0o700))?;
    fs::write(
        harness.state.join("host-namespace.json"),
        serde_json::to_vec(&json!({
            "boot_id": fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim(),
            "pid_namespace": fs::read_link("/proc/self/ns/pid")?.to_string_lossy(),
        }))?,
    )?;
    let spawn = |label: &str| -> Result<NamespaceChild, Box<dyn std::error::Error>> {
        Ok(NamespaceChild(
            Command::new("unshare")
                .args([
                    "--user",
                    "--map-root-user",
                    "--pid",
                    "--fork",
                    "--mount-proc",
                    "--kill-child=KILL",
                ])
                .arg(std::env::current_exe()?)
                .args([
                    "--exact",
                    "namespace_recovery_fixture",
                    "--ignored",
                    "--test-threads=4",
                ])
                .env("IX_BUILD_RECOVERY_STATE", &harness.state)
                .env("IX_BUILD_RECOVERY_CONFIG", &harness.config)
                .env(
                    "LOCKED_BUILD_FIXTURE_MARKER",
                    harness.root.path().join(label),
                )
                .env(
                    "LOCKED_BUILD_FIXTURE_RELEASE",
                    harness.root.path().join(format!("release-{label}")),
                )
                .env(
                    "DBUS_SESSION_BUS_ADDRESS",
                    "unix:path=/nonexistent-locked-build-test-bus",
                )
                .stdout(Stdio::from(fs::File::create(
                    harness.root.path().join(format!("{label}.stdout")),
                )?))
                .stderr(Stdio::from(fs::File::create(
                    harness.root.path().join(format!("{label}.stderr")),
                )?))
                .spawn()?,
        ))
    };
    let mut children = Vec::new();
    for label in ["dead", "live", "operator"] {
        let mut child = spawn(label)?;
        observed(|| {
            harness.root.path().join(label).exists()
                || child.0.try_wait().is_ok_and(|status| status.is_some())
        });
        assert!(
            harness.root.path().join(label).exists(),
            "{}\n{}",
            fs::read_to_string(harness.root.path().join(format!("{label}.stderr")))?,
            fs::read_to_string(harness.root.path().join(format!("{label}.stdout")))?
        );
        children.push(child);
    }
    let before = harness.report()?;
    assert_eq!(before["used_units"], 3);
    let live_id = before["requests"][1]["request_id"]
        .as_str()
        .ok_or("live id missing")?;
    let refused = harness
        .cli()
        .args([
            "recover",
            "--dry-run",
            "--release",
            live_id,
            "--reason",
            "fixture attempted live release",
        ])
        .output()?;
    assert_eq!(refused.status.code(), Some(2));
    let refused: Value = serde_json::from_slice(&refused.stdout)?;
    assert_eq!(refused["decisions"][0]["action"], "refused");
    assert!(
        !refused["decisions"][0]["live_evidence"]
            .as_array()
            .ok_or("evidence missing")?
            .is_empty()
    );
    for index in [0, 2] {
        children[index].0.kill()?;
        children[index].0.wait()?;
    }
    let dry = harness.cli().args(["recover", "--dry-run"]).output()?;
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let dry: Value = serde_json::from_slice(&dry.stdout)?;
    assert_eq!(dry["decisions"][0]["action"], "would_drain");
    assert_eq!(dry["decisions"][1]["action"], "retain");
    assert_eq!(dry["decisions"][2]["action"], "would_drain");
    let operator_id = before["requests"][2]["request_id"]
        .as_str()
        .ok_or("operator id missing")?;
    let released = harness
        .cli()
        .args([
            "recover",
            "--release",
            operator_id,
            "--reason",
            "fixture owner confirmed ended sandbox",
        ])
        .output()?;
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );
    let released: Value = serde_json::from_slice(&released.stdout)?;
    assert_eq!(released["decisions"][0]["action"], "released");
    assert!(harness.output("recover")?.status.success());
    let after = harness.report()?;
    assert_eq!(after["used_units"], 1);
    assert_eq!(after["requests"][0]["state"], "failed");
    assert_eq!(after["requests"][0]["code"], "LB_SUPERVISION_LOST");
    assert_eq!(after["requests"][1]["state"], "running");
    assert!(children[1].0.try_wait()?.is_none());
    assert!(
        after["events"]
            .as_array()
            .ok_or("events missing")?
            .iter()
            .any(|event| event["request_id"] == operator_id
                && event["kind"] == "operator_release"
                && event["code"] == "LB_SUPERVISION_LOST"
                && event["killed"] == "not-signalled/operator-release"
                && event["message"].as_str().is_some_and(|text| text
                    .contains("reason=fixture owner confirmed ended sandbox")
                    && text.contains(&format!("uid={}", nix::unistd::geteuid()))
                    && text.contains("prior_stop=None")))
    );
    let missing = harness.root.path().join("missing-state");
    let rejected = Command::new(env!("CARGO_BIN_EXE_locked-build"))
        .arg("--state")
        .arg(&missing)
        .arg("--config")
        .arg(&harness.config)
        .args(["recover", "--dry-run"])
        .output()?;
    assert_eq!(rejected.status.code(), Some(2));
    assert!(!missing.exists(), "dry-run error created a state directory");
    let invalid = Command::new(env!("CARGO_BIN_EXE_locked-build"))
        .env("LOCKED_BUILD_STATE", &missing)
        .args(["recover", "--dry-run=true"])
        .output()?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(
        !missing.exists(),
        "invalid dry-run flag created a state directory"
    );
    eprintln!(
        "namespace recovery: extinct drained; live retained; operator release audited; dry-run errors wrote no state"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "namespace recovery subprocess"]
fn namespace_recovery_fixture() -> TestResult {
    let marker = std::env::var_os("LOCKED_BUILD_FIXTURE_MARKER").ok_or("marker missing")?;
    let output = Command::new(env!("CARGO_BIN_EXE_locked-build"))
        .arg("--state")
        .arg(std::env::var_os("IX_BUILD_RECOVERY_STATE").ok_or("state missing")?)
        .arg("--config")
        .arg(std::env::var_os("IX_BUILD_RECOVERY_CONFIG").ok_or("config missing")?)
        .args([
            "run",
            "--owner",
            "test:namespace:worker",
            "--ticket",
            "SWM-421",
            "--",
        ])
        .arg(std::env::current_exe()?)
        .args(["--exact", "subprocess_fixture", "--ignored", "--skip"])
        .arg(marker)
        .env("LOCKED_BUILD_FIXTURE_MODE", "block")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Provenance: SWM-394, terra r244. Trace: FR-005-AC-4
#[cfg(target_os = "linux")]
#[test]
#[ignore = "explicit native Linux unshare --pid/no-user-bus regression"]
fn unshare_pid_namespace_keeps_supervision_through_child_exit() -> TestResult {
    let root = tempfile::tempdir()?;
    let stdout = root.path().join("namespace.stdout");
    let stderr = root.path().join("namespace.stderr");
    let mut namespace = NamespaceChild(
        Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--pid",
                "--fork",
                "--mount-proc",
                "--kill-child=KILL",
            ])
            .arg(std::env::current_exe()?)
            .args([
                "--exact",
                "namespace_local_watchdog_survives_mutation_style_child_churn",
                "--ignored",
                "--nocapture",
                "--test-threads=4",
            ])
            .env("IX_BUILD_PID_NAMESPACE_FIXTURE", "1")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-locked-build-test-bus",
            )
            .env("XDG_RUNTIME_DIR", "/nonexistent-locked-build-test-runtime")
            .env("TMPDIR", root.path())
            .stdout(Stdio::from(fs::File::create(&stdout)?))
            .stderr(Stdio::from(fs::File::create(&stderr)?))
            .spawn()?,
    );
    let child = &mut namespace.0;
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let mut failed = false;
        for entry in fs::read_dir(root.path())? {
            let state = entry?.path().join("state/state.json");
            if state.is_file() {
                let bytes = match fs::read(state) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let state: Value = serde_json::from_slice(&bytes)?;
                for request in state["requests"].as_array().into_iter().flatten() {
                    if request["supervision_detail"].is_string() {
                        eprintln!(
                            "namespace supervision failure: worker={} watchdog={} command={} phase={} detail={}",
                            request["worker"],
                            request["watchdog"],
                            request["lease_record"]["command"],
                            request["phase"],
                            request["supervision_detail"]
                        );
                        failed = true;
                    }
                }
            }
        }
        if failed || Instant::now() >= deadline {
            // unshare owns the namespace init; killing its owned Child handle
            // with --kill-child also reaps all namespace-local process groups.
            child.kill()?;
            let status = child.wait()?;
            for entry in fs::read_dir(root.path())? {
                let state = entry?.path().join("state/state.json");
                if state.is_file() {
                    let state: Value = serde_json::from_slice(&fs::read(state)?)?;
                    for request in state["requests"].as_array().into_iter().flatten() {
                        eprintln!(
                            "namespace timeout: request={} worker={} watchdog={} phase={} detail={}",
                            request["id"],
                            request["worker"],
                            request["watchdog"],
                            request["phase"],
                            request["supervision_detail"]
                        );
                    }
                }
            }
            break status;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        status.success(),
        "unshare PID fixture failed (not skipped):\n{}\n{}",
        fs::read_to_string(stdout)?,
        fs::read_to_string(stderr)?
    );
    eprintln!("{}", fs::read_to_string(stderr)?);
    Ok(())
}

/// Provenance: SWM-394, terra r244. Trace: FR-005-AC-4
#[cfg(target_os = "linux")]
#[test]
#[ignore = "native Linux PID namespace/no-user-bus churn lane"]
fn namespace_local_watchdog_survives_mutation_style_child_churn() -> TestResult {
    let harness = Harness::new(1, 120000)?;
    let mut policy: Value = serde_json::from_slice(&fs::read(&harness.config)?)?;
    policy["memory_admission"] = json!("lease_accounted");
    policy["memory_budget_bytes"] = json!(16_000_000_000_u64);
    policy["memory_limit_per_job_bytes"] = json!(8_000_000_000_u64);
    fs::write(&harness.config, serde_json::to_vec(&policy)?)?;
    let binary = std::env::var_os("IX_BUILD_TEST_INSTALLED_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_locked-build").into());
    let marker = harness.root.path().join("churn");
    let output = Command::new(binary)
        .arg("--state")
        .arg(&harness.state)
        .arg("--config")
        .arg(&harness.config)
        .args([
            "run",
            "--owner",
            "namespace-churn",
            "--ticket",
            "SWM-394",
            "--",
        ])
        .arg(std::env::current_exe()?)
        .args([
            "--exact",
            "subprocess_fixture",
            "--ignored",
            "--nocapture",
            "--skip",
        ])
        .arg(&marker)
        .env("LOCKED_BUILD_FIXTURE_MODE", "churn")
        .env("LOCKED_BUILD_FIXTURE_MARKER", &marker)
        .env(
            "LOCKED_BUILD_FIXTURE_RELEASE",
            harness.root.path().join("unused"),
        )
        .env_remove("CARGO_BUILD_JOBS")
        .env_remove("RUST_TEST_THREADS")
        .stdout(Stdio::null())
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = harness.request(0)?;
    assert_eq!(request["state"], "succeeded");
    assert_eq!(request["exit_code"], 0);
    assert!(request["code"].is_null());
    assert_eq!(
        request["lease_record"]["memory_enforcement"],
        "rss_watchdog"
    );
    assert_eq!(
        request["pid_namespace"],
        fs::read_link("/proc/self/ns/pid")?
            .to_string_lossy()
            .as_ref()
    );
    if std::env::var_os("IX_BUILD_PID_NAMESPACE_FIXTURE").is_some() {
        let state: Value = serde_json::from_slice(&fs::read(harness.state.join("state.json"))?)?;
        let request = &state["requests"][0];
        for role in ["worker", "watchdog"] {
            assert!(
                request[role]["pid"]
                    .as_u64()
                    .is_some_and(|pid| (2..128).contains(&pid)),
                "expected namespace-local {role} PID: {request}"
            );
            assert!(
                request[role]["started_seconds"]
                    .as_u64()
                    .is_some_and(|start| start > 0)
            );
        }
        assert!(
            request["lease_record"]["command"]["pid"]
                .as_u64()
                .is_some_and(|pid| (2..128).contains(&pid))
        );
        eprintln!(
            "namespace-local lease survived: worker={} watchdog={} command={} namespace={}",
            request["worker"]["pid"],
            request["watchdog"]["pid"],
            request["lease_record"]["command"]["pid"],
            request["pid_namespace"]
        );
    }
    Ok(())
}

/// Provenance: SWM-391. Trace: FR-005-AC-4
#[test]
fn scheduler_restarts_while_queued_workers_live_and_status_ignores_a_stuck_writer() -> TestResult {
    let harness = Harness::new(0, 15000)?;
    let marker = harness.root.path().join("outer");
    let release = harness.root.path().join("release");
    let mut child = harness.command("block", &marker, &release, 1)?;
    let owner = || -> Option<u32> {
        let bytes = fs::read(harness.state.join("scheduler.lock")).ok()?;
        Some(u32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?))
    };
    observed(|| owner().is_some());
    let original = owner().ok_or("scheduler owner absent")?;
    kill(Pid::from_raw(i32::try_from(original)?), Signal::SIGKILL)?;
    observed(|| owner().is_some_and(|pid| pid != original));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(harness.state.join("lock"))?;
    lock.lock()?;
    let started = Instant::now();
    let output = harness.cli().arg("status").output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(lock);
    let mut policy: Value = serde_json::from_slice(&fs::read(&harness.config)?)?;
    policy["capacity_units"] = json!(1);
    fs::write(&harness.config, serde_json::to_vec(&policy)?)?;
    let started = Instant::now();
    observed(|| marker.exists());
    assert!(started.elapsed() < Duration::from_secs(3));
    fs::write(release, "release")?;
    assert!(complete(&mut child)?.success());
    assert_eq!(fs::read_to_string(marker)?, "invoked\n");
    Ok(())
}

/// Provenance: SWM-392. Trace: FR-011-AC-7, FR-005-AC-4
#[test]
fn nested_shim_failure_never_signals_or_readmits_the_outer_lease() -> TestResult {
    let harness = Harness::new(1, 15000)?;
    let shim = harness.root.path().join("shim");
    fs::create_dir(&shim)?;
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_locked-build"), shim.join("cargo"))?;
    let mut policy: Value = serde_json::from_slice(&fs::read(&harness.config)?)?;
    policy["real_cargo"] = json!("/usr/bin/false");
    policy["memory_admission"] = json!("lease_accounted");
    policy["memory_budget_bytes"] = json!(8_000_000_000_u64);
    fs::write(&harness.config, serde_json::to_vec(&policy)?)?;
    let marker = harness.root.path().join("outer");
    let release = harness.root.path().join("survived");
    let mut child = harness
        .cli()
        .arg("run")
        .args(["--owner", "nested-fixture", "--ticket", "SWM-392", "--"])
        .arg(std::env::current_exe()?)
        .args(["--exact", "subprocess_fixture", "--ignored", "--nocapture"])
        .env("LOCKED_BUILD_FIXTURE_MODE", "nested-failure")
        .env("LOCKED_BUILD_FIXTURE_MARKER", &marker)
        .env("LOCKED_BUILD_FIXTURE_RELEASE", &release)
        .env(
            "PATH",
            std::env::join_paths([shim, PathBuf::from("/usr/bin"), PathBuf::from("/bin")])?,
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    assert!(complete(&mut child)?.success());
    assert_eq!(
        fs::read_to_string(&release)?,
        "inner failed but outer survived"
    );
    let state = harness.persisted()?;
    assert_eq!(state["requests"].as_array().map(Vec::len), Some(1));
    assert_eq!(state["requests"][0]["phase"], "succeeded");
    assert!(!harness.state.join("failures.jsonl").exists());
    Ok(())
}

struct Harness {
    root: TempDir,
    state: PathBuf,
    config: PathBuf,
    sample: PathBuf,
    sample_stop: Arc<AtomicBool>,
    sample_lock: Arc<Mutex<()>>,
    sample_refresher: Option<std::thread::JoinHandle<()>>,
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
                "disk_path": root.path(), "maximum_load": 100.0,
                "aging_ms": 1000, "hold_ms": hold_ms, "termination_grace_ms": 200,
                "sample_file": sample,
                "memory_admission": "live_available", "memory_budget_bytes": null,
                "memory_reserve_per_job_bytes": 2500000000_u64, "memory_limit_per_job_bytes": 2500000000_u64, "memory_per_job_by_class": {}, "memory_cap_by_class": {},
                "on_missing_cgroup": "rss_watchdog",
            }))?,
        )?;
        let sample_stop = Arc::new(AtomicBool::new(false));
        let sample_lock = Arc::new(Mutex::new(()));
        let refresher_stop = sample_stop.clone();
        let refresher_lock = sample_lock.clone();
        let refresher_path = sample.clone();
        let sample_refresher = Some(std::thread::spawn(move || {
            while !refresher_stop.load(Ordering::Relaxed) {
                if let Ok(_guard) = refresher_lock.lock() {
                    let _ = write_sample(&refresher_path);
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }));
        Ok(Self {
            root,
            state,
            config,
            sample,
            sample_stop,
            sample_lock,
            sample_refresher,
        })
    }

    fn refresh_sample(&self) -> Result<(), Box<dyn std::error::Error>> {
        let _guard = self
            .sample_lock
            .lock()
            .map_err(|_| std::io::Error::other("sample refresh lock poisoned"))?;
        write_sample(&self.sample)
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
        self.command_with_stderr(mode, marker, release, units, Stdio::null())
    }

    fn command_with_stderr(
        &self,
        mode: &str,
        marker: &Path,
        release: &Path,
        units: u32,
        stderr: Stdio,
    ) -> Result<Child, Box<dyn std::error::Error>> {
        self.refresh_sample()?;
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
            .stderr(stderr)
            .spawn()?)
    }

    fn output(&self, action: &str) -> Result<Output, Box<dyn std::error::Error>> {
        self.refresh_sample()?;
        Ok(self.cli().arg(action).output()?)
    }

    fn report(&self) -> Result<Value, Box<dyn std::error::Error>> {
        self.refresh_sample()?;
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
        self.refresh_sample()?;
        Ok(serde_json::from_slice(&fs::read(
            self.state.join("state.json"),
        )?)?)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.sample_stop.store(true, Ordering::Relaxed);
        if let Some(refresher) = self.sample_refresher.take() {
            let _ = refresher.join();
        }
        let mut pids = std::collections::BTreeSet::new();
        if let Ok(bytes) = fs::read(self.state.join("state.json"))
            && let Ok(state) = serde_json::from_slice::<Value>(&bytes)
        {
            for request in state["requests"].as_array().into_iter().flatten() {
                if let Some(pid) = request["lease_record"]["command"]["pid"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok())
                {
                    pids.insert(pid);
                }
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
                let pid = Pid::from_raw(pid);
                if nix::unistd::getpgid(Some(pid)) == Ok(pid) {
                    let _ = nix::sys::signal::killpg(pid, Signal::SIGKILL);
                } else {
                    let _ = kill(pid, Signal::SIGKILL);
                }
            }
        }
    }
}

fn write_sample(sample: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let next = sample.with_extension("json.next");
    fs::write(
        &next,
        serde_json::to_vec(&json!({
            "host": "fixture-host", "observed_at_ms": now,
            "memory_available_bytes": 1000000, "disk_available_bytes": 1000000, "load_one": 0.0,
        }))?,
    )?;
    fs::rename(next, sample)?;
    Ok(())
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
    let harness = Harness::new(1, 120000)?;
    let marker = harness.root.path().join("invocations");
    let release = harness.root.path().join("release");
    let mut child = harness.command("exit75", &marker, &release, 1)?;
    assert_eq!(complete(&mut child)?.code(), Some(75));
    assert_eq!(fs::read_to_string(&marker)?, "invoked\n");
    let report = harness.report()?;
    assert_eq!(report["requests"][0]["state"], "failed");
    assert_eq!(report["requests"][0]["exit_code"], 75);
    assert_eq!(report["events"].as_array().map(Vec::len), Some(4));
    assert_eq!(report["events"][2]["kind"], "command_started");
    assert_eq!(report["used_units"], 0);
    assert!(!serde_json::to_string(&report)?.contains("subprocess_fixture"));
    Ok(())
}

/// Trace: FR-005-AC-4, FR-005-AC-7
#[test]
fn exit_75_survives_forced_descendant_cleanup_and_reaps_its_watchdog() -> TestResult {
    let harness = Harness::new(1, 120000)?;
    let marker = harness.root.path().join("invocations");
    let descendant = marker.with_extension("descendant");
    let mut child = harness.command(
        "exit75-descendant",
        &marker,
        &harness.root.path().join("never-release"),
        1,
    )?;
    assert_eq!(
        complete_before(&mut child, Duration::from_secs(30))?.code(),
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
    let harness = Harness::new(1, 120000)?;
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
    let harness = Harness::new(1, 120000)?;
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
        .filter(|event| event["state"] == "running" && event["kind"] == "transition")
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
    let harness = Harness::new(0, 120000)?;
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
    let harness = Harness::new(1, 3000)?;
    let marker = harness.root.path().join("started");
    let child = harness.command_with_stderr(
        "ignore-term",
        &marker,
        &harness.root.path().join("never-release"),
        1,
        Stdio::piped(),
    )?;
    observed(|| marker.exists());
    let descendant = marker.with_extension("descendant");
    observed(|| descendant.exists());
    assert_eq!(harness.report()?["used_units"], 1);
    let output = child.wait_with_output()?;
    assert_eq!(output.status.code(), Some(124));
    let run_output = String::from_utf8(output.stderr)?;
    let diagnostics = run_output
        .lines()
        .filter(|line| line.starts_with("locked-build: "))
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 1, "{run_output}");
    assert!(
        diagnostics[0].starts_with("locked-build: LB_HOLD_TIMEOUT request="),
        "{run_output}"
    );
    assert!(
        diagnostics[0].contains(" limit=policy.hold_ms=3000 measured="),
        "{run_output}"
    );
    assert!(
        diagnostics[0].contains(" (elapsed_ms) killed=pgid:"),
        "{run_output}"
    );
    assert_eq!(harness.request(0)?["state"], "timed_out");
    let request = harness.request(0)?;
    assert_eq!(request["code"], "LB_HOLD_TIMEOUT");
    assert_eq!(request["message"], diagnostics[0]);
    let reason = request["reason"]
        .as_str()
        .ok_or("reason missing")?
        .to_owned();
    assert!(reason.contains("hold_ms=3000 ms"), "{reason}");
    assert!(
        reason.contains("change the limit in policy hold_ms"),
        "{reason}"
    );
    assert_eq!(harness.report()?["used_units"], 0);
    assert!(!fixture_alive(&marker)?);
    assert!(!fixture_alive(&descendant)?);
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn supervisor_crash_keeps_lease_until_its_child_group_is_verified_stopped() -> TestResult {
    let harness = Harness::new(1, 120000)?;
    let marker = harness.root.path().join("started");
    let release = harness.root.path().join("release");
    let mut child = harness.command("block", &marker, &release, 1)?;
    observed(|| marker.exists());
    let state = harness.persisted()?;
    let pid = state["requests"][0]["worker"]["pid"]
        .as_u64()
        .ok_or("PID missing")?;
    #[cfg(target_os = "linux")]
    {
        let watchdog = state["requests"][0]["watchdog"]["pid"]
            .as_u64()
            .ok_or("watchdog missing")?;
        assert_eq!(
            fs::read_link(format!("/proc/{watchdog}/fd/0"))?,
            PathBuf::from("/dev/null")
        );
    }
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
    let harness = Harness::new(1, 120000)?;
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
    let harness = Harness::new(2, 120000)?;
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
    let harness = Harness::new(0, 120000)?;
    let marker = harness.root.path().join("started");
    let release = harness.root.path().join("release");
    let mut child = harness.command("block", &marker, &release, 1)?;
    observed(|| {
        harness.persisted().is_ok_and(|state| {
            let request = &state["requests"][0];
            let Some(id) = request["id"].as_str() else {
                return false;
            };
            fs::read(harness.state.join(format!("{id}.launch.json")))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|claim| {
                    claim["worker"]["pid"].is_u64() && claim["worker"] == request["worker"]
                })
        })
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
    let harness = Harness::new(1, 120000)?;
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
    let harness = Harness::new(1, 120000)?;
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
    assert_eq!(harness.report()?["pruned_events"], 4);
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn running_cancel_returns_cancelled_after_group_cleanup() -> TestResult {
    let harness = Harness::new(1, 120000)?;
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

/// Provenance: SWM-394.
/// Trace: FR-005-AC-4, FR-005-AC-7
#[test]
fn cancelling_a_compiling_cargo_lease_terms_then_kills_and_reaps_the_group() -> TestResult {
    let harness = Harness::new(1, 120000)?;
    let mut policy: Value = serde_json::from_slice(&fs::read(&harness.config)?)?;
    policy["termination_grace_ms"] = json!(1500);
    policy["memory_admission"] = json!("lease_accounted");
    policy["memory_budget_bytes"] = json!(16_000_000_000_u64);
    fs::write(&harness.config, serde_json::to_vec(&policy)?)?;
    let project = harness.root.path().join("compiling");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"cancel-compile-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n",
    )?;
    fs::write(project.join("src/lib.rs"), "pub fn compiled() {}\n")?;
    fs::write(
        project.join("build.rs"),
        include_str!("fixtures/cancel-build.rs"),
    )?;
    let marker = harness.root.path().join("compiler");
    let descendant = marker.with_extension("descendant");
    let stderr = harness.root.path().join("caller.stderr");
    let mut child = harness
        .cli()
        .arg("run")
        .args(["--owner", "cancel-compilation", "--ticket", "SWM-394", "--"])
        .arg(env!("CARGO"))
        .args(["test", "--offline", "-j", "1", "--manifest-path"])
        .arg(project.join("Cargo.toml"))
        .args(["--", "--test-threads=4"])
        .env("CARGO_TARGET_DIR", project.join("target"))
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env("LOCKED_BUILD_CANCEL_FIXTURE", std::env::current_exe()?)
        .env("LOCKED_BUILD_FIXTURE_MARKER", &marker)
        .env(
            "LOCKED_BUILD_FIXTURE_RELEASE",
            harness.root.path().join("never-release"),
        )
        .stdout(Stdio::null())
        .stderr(fs::File::create(&stderr)?)
        .spawn()?;
    observed(|| {
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => panic!(
                "compiling lease exited before cancellation: {status}: {}",
                fs::read_to_string(&stderr).unwrap_or_default()
            ),
            Err(error) => panic!("fixture runner wait failed: {error}"),
        }
        marker.exists() && descendant.exists()
    });
    let request = harness.request(0)?;
    assert_eq!(request["state"], "running");
    let id = request["request_id"].as_str().ok_or("request ID missing")?;
    let cargo_pid = i32::try_from(
        request["lease_record"]["command"]["pid"]
            .as_u64()
            .ok_or("Cargo PID missing")?,
    )?;
    let compiler_pid: i32 = fs::read_to_string(pid_file(&marker))?.parse()?;
    let descendant_pid: i32 = fs::read_to_string(pid_file(&descendant))?.parse()?;
    let group = nix::unistd::getpgid(Some(Pid::from_raw(compiler_pid)))?;
    let cancelled_at = Instant::now();
    assert!(
        harness
            .cli()
            .args(["cancel", id])
            .output()?
            .status
            .success()
    );
    observed(|| {
        pid_file(&marker).with_extension("term").exists()
            && pid_file(&descendant).with_extension("term").exists()
    });
    // Both TERM-resistant members must remain alive during the grace period.
    let grace_observation = Instant::now();
    while grace_observation.elapsed() < Duration::from_millis(750) {
        assert_eq!(kill(Pid::from_raw(compiler_pid), None), Ok(()));
        assert_eq!(kill(Pid::from_raw(descendant_pid), None), Ok(()));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(complete(&mut child)?.code(), Some(130));
    assert!(
        cancelled_at.elapsed() >= Duration::from_millis(1500),
        "KILL cleanup finished before the configured TERM grace"
    );
    assert_eq!(
        nix::sys::signal::killpg(group, None),
        Err(nix::errno::Errno::ESRCH)
    );
    for pid in [cargo_pid, compiler_pid, descendant_pid] {
        assert_eq!(
            kill(Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH),
            "PID {pid} was not reaped"
        );
    }
    let report = harness.report()?;
    let request = &report["requests"][0];
    assert_eq!(request["state"], "cancelled");
    assert_eq!(request["code"], "LB_CANCELLED");
    assert_eq!(report["used_units"], 0);
    let caller = fs::read_to_string(stderr)?;
    assert_eq!(
        caller
            .lines()
            .filter(|line| line.starts_with("locked-build: "))
            .collect::<Vec<_>>(),
        vec![request["message"].as_str().ok_or("diagnostic missing")?]
    );
    assert!(
        !caller.contains("watchdog supervision unavailable"),
        "{caller}"
    );
    let worker_output = fs::read_to_string(harness.state.join(format!("{id}.stderr")))?;
    assert!(
        !worker_output.contains("watchdog supervision unavailable"),
        "{worker_output}"
    );
    assert!(
        report["events"]
            .as_array()
            .ok_or("events missing")?
            .iter()
            .any(|event| event["request_id"] == id
                && event["code"] == "LB_CANCELLED"
                && event["message"] == request["message"])
    );
    Ok(())
}

/// Trace: FR-005-AC-4
#[test]
fn cancellation_after_supervisor_loss_stops_owned_descendants_and_preserves_unrelated_work()
-> TestResult {
    let harness = Harness::new(1, 120000)?;
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
        complete_before(&mut child, Duration::from_secs(30))?.code(),
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
    let harness = Harness::new(1, 15000)?;
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
        complete_before(&mut child, Duration::from_secs(30))?.code(),
        Some(124)
    );
    assert_eq!(harness.request(0)?["state"], "timed_out");
    let request = harness.request(0)?;
    let reason = request["reason"]
        .as_str()
        .ok_or("reason missing")?
        .to_owned();
    assert!(reason.contains("hold_timeout"), "{reason}");
    assert!(reason.contains("hold_ms=15000 ms"), "{reason}");
    assert!(
        reason.contains("change the limit in policy hold_ms"),
        "{reason}"
    );
    assert!(!fixture_alive(&marker)?);
    assert!(!fixture_alive(&descendant)?);
    assert_eq!(harness.report()?["used_units"], 0);
    Ok(())
}

#[test]
#[ignore = "subprocess workload, invoked by supervision tests"]
fn subprocess_fixture() -> TestResult {
    use std::os::fd::AsFd;
    use std::os::unix::fs::FileTypeExt;
    let stdin: fs::File = std::io::stdin().as_fd().try_clone_to_owned()?.into();
    assert!(
        stdin.metadata()?.file_type().is_char_device(),
        "workload inherited a coordination lock as stdin"
    );
    let mode = std::env::var("LOCKED_BUILD_FIXTURE_MODE")?;
    let marker =
        PathBuf::from(std::env::var_os("LOCKED_BUILD_FIXTURE_MARKER").ok_or("marker missing")?);
    let release =
        PathBuf::from(std::env::var_os("LOCKED_BUILD_FIXTURE_RELEASE").ok_or("release missing")?);
    let terminated = Arc::new(AtomicBool::new(false));
    if matches!(
        mode.as_str(),
        "ignore-term" | "leaf-ignore-term" | "cancel-compiler" | "cancel-child"
    ) {
        signal_hook::flag::register(signal_hook::consts::SIGTERM, terminated.clone())?;
    }
    if matches!(
        mode.as_str(),
        "ignore-term" | "exit75-descendant" | "cancel-compiler"
    ) {
        let descendant = marker.with_extension("descendant");
        Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("subprocess_fixture")
            .arg("--ignored")
            .arg("--nocapture")
            .arg("--skip")
            .arg(&descendant)
            .env(
                "LOCKED_BUILD_FIXTURE_MODE",
                if mode == "cancel-compiler" {
                    "cancel-child"
                } else {
                    "leaf-ignore-term"
                },
            )
            .env("LOCKED_BUILD_FIXTURE_MARKER", &descendant)
            .spawn()?;
        observed(|| descendant.exists());
    }
    fs::write(pid_file(&marker), std::process::id().to_string())?;
    let mut file = OpenOptions::new().create(true).append(true).open(&marker)?;
    writeln!(file, "invoked")?;
    file.sync_all()?;
    match mode.as_str() {
        "churn" => {
            for _ in 0..12_000 {
                assert!(Command::new("/usr/bin/true").status()?.success());
            }
        }
        "nested-failure" => {
            use std::os::unix::process::ExitStatusExt;
            let output = Command::new("cargo").arg("inner-failure").output()?;
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(output.status.signal(), None);
            assert!(output.stderr.is_empty());
            fs::write(&release, "inner failed but outer survived")?;
        }
        "exit75" | "exit75-descendant" => std::process::exit(75),
        "signal" => {
            kill(Pid::this(), Signal::SIGTERM)?;
        }
        "block" | "ignore-term" | "leaf-ignore-term" | "cancel-compiler" | "cancel-child" => {
            while !release.exists() {
                if terminated.swap(false, Ordering::Relaxed) {
                    fs::write(pid_file(&marker).with_extension("term"), "TERM observed")?;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        _ => return Err("unknown fixture mode".into()),
    }
    Ok(())
}
