// SPDX-License-Identifier: MIT
//! External-wrapper observation behavior.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::sys::stat::Mode;
use nix::unistd::{Pid, mkfifo};
use serde_json::json;
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    root: TempDir,
    state: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new(capacity: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("state");
        fs::create_dir(&state)?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        let config = root.path().join("policy.json");
        fs::write(
            &config,
            serde_json::to_vec(&json!({
                "host": "observe-test", "capacity_units": capacity, "maximum_units": 4,
                "memory_per_unit_bytes": 100, "memory_reserve_bytes": 0, "disk_reserve_bytes": 0,
                "disk_path": root.path(), "maximum_load": 100.0, "maximum_priority": 2,
                "aging_ms": 1000, "hold_ms": 30000, "termination_grace_ms": 1000,
                "sample_max_age_ms": 60000, "heartbeat_max_age_ms": 10000, "poll_ms": 10,
                "maximum_requests": 100, "sample_file": null
            }))?,
        )?;
        Ok(Self {
            root,
            state,
            config,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_locked-build"));
        command
            .arg("--state")
            .arg(&self.state)
            .arg("--config")
            .arg(&self.config);
        command
    }

    fn observe(&self, hints: &[&str], args: &[&str]) -> std::io::Result<std::process::Output> {
        let mut command = self.command();
        command
            .arg("observe")
            .args(hints)
            .arg("--")
            .args(args)
            .output()
    }

    fn admitted(&self) -> std::io::Result<std::process::Output> {
        self.command()
            .arg("run")
            .arg("--owner")
            .arg("test:team:lead")
            .arg("--ticket")
            .arg("SWM-242")
            .arg("--")
            .arg("/usr/bin/true")
            .output()
    }
}

/// Trace: FR-015-AC-13
#[test]
fn observe_runs_immediately_when_configured_capacity_is_zero() -> TestResult {
    let fixture = Fixture::new(0)?;
    let marker = fixture.root.path().join("ran");
    let output = fixture.observe(&[], &["/usr/bin/touch", marker.to_str().ok_or("path")?])?;
    assert!(output.status.success());
    assert!(marker.exists());
    Ok(())
}

/// Trace: FR-015-AC-14
#[test]
fn missing_state_fails_open_with_one_warning_and_no_state_creation() -> TestResult {
    let fixture = Fixture::new(1)?;
    let missing = fixture.root.path().join("missing");
    let marker = fixture.root.path().join("ran");
    let mut command = Command::new(env!("CARGO_BIN_EXE_locked-build"));
    let output = command
        .arg("--state")
        .arg(&missing)
        .arg("--config")
        .arg(&fixture.config)
        .arg("observe")
        .arg("--")
        .arg("/usr/bin/touch")
        .arg(&marker)
        .output()?;
    assert!(output.status.success());
    assert!(marker.exists());
    assert!(!missing.exists());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    Ok(())
}

/// Trace: FR-015-AC-15
#[test]
fn observe_preserves_command_output_environment_and_working_directory() -> TestResult {
    let fixture = Fixture::new(1)?;
    let output = fixture
        .command()
        .env("SWM_OBSERVE_TEST", "preserved")
        .arg("observe")
        .arg("--")
        .arg("/usr/bin/env")
        .output()?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    let cwd = std::env::current_dir()?;
    assert!(text.contains("SWM_OBSERVE_TEST=preserved\n"));
    let pwd = fixture.observe(&[], &["/bin/pwd"])?;
    assert_eq!(String::from_utf8(pwd.stdout)?.trim(), cwd.to_string_lossy());
    assert_eq!(output.stderr, Vec::<u8>::new());

    let mut command = fixture
        .command()
        .arg("observe")
        .arg("--")
        .arg("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    command
        .stdin
        .take()
        .ok_or("child stdin")?
        .write_all(b"stdin-forwarded")?;
    let echoed = command.wait_with_output()?;
    assert_eq!(echoed.stdout, b"stdin-forwarded");
    assert!(echoed.status.success());

    let stderr = fixture.observe(&[], &["/bin/ls", "/path/that/does/not/exist"])?;
    assert_eq!(stderr.status.code(), Some(1));
    assert!(stderr.stdout.is_empty());
    assert!(!stderr.stderr.is_empty());
    Ok(())
}

/// Trace: FR-015-AC-16
#[test]
fn busy_slot_is_released_and_configured_slots_follow_host_policy() -> TestResult {
    let fixture = Fixture::new(3)?;
    let output = fixture.observe(&[], &["/usr/bin/true"])?;
    assert!(output.status.success());
    let metrics = fixture.command().arg("metrics").output()?;
    assert!(
        metrics.status.success(),
        "{}",
        String::from_utf8_lossy(&metrics.stderr)
    );
    let text = String::from_utf8(metrics.stdout)?;
    assert!(text.contains("ix_build_slots_configured{host=\"observe-test\"} 3"));
    assert!(text.contains("ix_build_slots_busy{host=\"observe-test\"} 0"));
    Ok(())
}

/// Trace: FR-015-AC-17
#[test]
fn outcome_metrics_require_explicit_kind_and_build_priority() -> TestResult {
    let fixture = Fixture::new(2)?;
    assert!(
        fixture
            .observe(
                &["--kind", "build", "--priority", "check"],
                &["/usr/bin/true"]
            )?
            .status
            .success()
    );
    assert!(fixture.observe(&[], &["/usr/bin/false"])?.status.code() == Some(1));
    let output = fixture.command().arg("metrics").output()?;
    let text = String::from_utf8(output.stdout)?;
    assert!(text.contains(
        "ix_build_builds_total{host=\"observe-test\",priority=\"check\",outcome=\"ok\"} 1"
    ));
    assert!(!text.contains(
        "ix_build_builds_total{host=\"observe-test\",priority=\"check\",outcome=\"failed\"} 1"
    ));
    assert!(!text.contains("ix_build_queue_depth"));
    assert!(!text.contains("ix_build_admission_blocked_seconds_total"));
    assert!(!text.contains("ix_build_pause_seconds_total"));
    Ok(())
}

/// Trace: FR-015-AC-18
#[test]
fn priority_is_only_an_explicit_attribution_hint() -> TestResult {
    let fixture = Fixture::new(2)?;
    assert!(
        fixture
            .observe(&["--kind", "build"], &["/usr/bin/true"])?
            .status
            .success()
    );
    let output = fixture.command().arg("metrics").output()?;
    let text = String::from_utf8(output.stdout)?;
    assert!(
        text.lines()
            .filter(|line| line.starts_with("ix_build_builds_total"))
            .all(|line| line.ends_with(" 0"))
    );
    Ok(())
}

/// Trace: FR-015-AC-19
#[test]
fn queue_wait_requires_both_queue_timestamp_and_priority() -> TestResult {
    let fixture = Fixture::new(1)?;
    assert!(
        fixture
            .observe(
                &["--kind", "test", "--queued-at-ms", "1"],
                &["/usr/bin/true"]
            )?
            .status
            .success()
    );
    assert!(
        fixture
            .observe(&["--priority", "test"], &["/usr/bin/true"])?
            .status
            .success()
    );
    let output = fixture.command().arg("metrics").output()?;
    assert!(!String::from_utf8(output.stdout)?.contains("ix_build_queue_wait_seconds"));
    Ok(())
}

/// Trace: FR-015-AC-20
#[test]
fn observer_export_omits_unknown_scheduler_metrics() -> TestResult {
    let fixture = Fixture::new(1)?;
    assert!(fixture.observe(&[], &["/usr/bin/true"])?.status.success());
    let output = fixture.command().arg("metrics").output()?;
    let text = String::from_utf8(output.stdout)?;
    assert!(!text.contains("ix_build_queue_depth"));
    assert!(!text.contains("ix_build_admission_blocked_seconds_total"));
    assert!(!text.contains("ix_build_pause_seconds_total"));
    Ok(())
}

/// Trace: FR-015-AC-15
#[test]
fn group_sigint_sigterm_sighup_reach_observed_command() -> TestResult {
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        let fixture = Fixture::new(1)?;
        let mut child = fixture
            .command()
            .arg("observe")
            .arg("--")
            .arg("/bin/sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        wait_for_command_identity(&fixture.state, child.id())?;
        killpg(Pid::from_raw(i32::try_from(child.id())?), signal)?;
        let status = child.wait()?;
        assert_eq!(status.signal(), Some(signal as i32), "{signal}");
    }
    Ok(())
}

/// Trace: FR-015-AC-14
#[test]
fn fifo_policy_fails_open_without_blocking() -> TestResult {
    let fixture = Fixture::new(1)?;
    let fifo = fixture.root.path().join("policy-fifo");
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR)?;
    let marker = fixture.root.path().join("ran-fifo");
    let mut command = Command::new(env!("CARGO_BIN_EXE_locked-build"));
    let started = std::time::Instant::now();
    let output = command
        .arg("--state")
        .arg(&fixture.state)
        .arg("--config")
        .arg(&fifo)
        .arg("observe")
        .arg("--")
        .arg("/usr/bin/touch")
        .arg(&marker)
        .output()?;
    assert!(output.status.success());
    assert!(marker.exists());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    assert!(!fixture.state.join("observe.json").exists());
    Ok(())
}

/// Trace: FR-015-AC-14
#[test]
fn invalid_policy_fails_open_once_without_writing_observer_state() -> TestResult {
    let fixture = Fixture::new(1)?;
    fs::write(&fixture.config, b"not json")?;
    let marker = fixture.root.path().join("ran-bad-config");
    let output = fixture.observe(&[], &["/usr/bin/touch", marker.to_str().ok_or("path")?])?;
    assert!(output.status.success());
    assert!(marker.exists());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    assert!(!fixture.state.join("observe.json").exists());
    Ok(())
}

/// Trace: FR-015-AC-14
#[test]
fn unwritable_state_directory_fails_open_when_permissions_are_enforced() -> TestResult {
    let fixture = Fixture::new(1)?;
    fs::set_permissions(&fixture.state, fs::Permissions::from_mode(0o500))?;
    let probe = fixture.state.join("permission-probe");
    if fs::File::create(&probe).is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(&fixture.state, fs::Permissions::from_mode(0o700))?;
        return Ok(());
    }
    let marker = fixture.root.path().join("ran-unwritable");
    let output = fixture.observe(&[], &["/usr/bin/touch", marker.to_str().ok_or("path")?])?;
    assert!(output.status.success());
    assert!(marker.exists());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    assert!(!fixture.state.join("observe.json").exists());
    Ok(())
}

/// Trace: FR-015-AC-14
#[test]
fn held_state_lock_fails_open_after_the_bounded_wait() -> TestResult {
    let fixture = Fixture::new(1)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(fixture.state.join("lock"))?;
    lock.lock()?;
    let marker = fixture.root.path().join("ran-lock");
    let started = std::time::Instant::now();
    let output = fixture.observe(&[], &["/usr/bin/touch", marker.to_str().ok_or("path")?])?;
    assert!(output.status.success());
    assert!(marker.exists());
    assert!(started.elapsed() < Duration::from_millis(1_500));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    assert!(!fixture.state.join("observe.json").exists());
    Ok(())
}

/// Trace: FR-015-AC-14
#[test]
fn corrupt_observed_state_fails_open_without_replacing_it() -> TestResult {
    let fixture = Fixture::new(1)?;
    let state = fixture.state.join("observe.json");
    fs::write(&state, b"{broken")?;
    let marker = fixture.root.path().join("ran-corrupt");
    let output = fixture.observe(&[], &["/usr/bin/touch", marker.to_str().ok_or("path")?])?;
    assert!(output.status.success());
    assert!(marker.exists());
    assert_eq!(fs::read(state)?, b"{broken");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    Ok(())
}

/// Trace: FR-015-AC-14
#[test]
fn admitted_mode_mismatch_fails_open_without_observer_writes() -> TestResult {
    let fixture = Fixture::new(1)?;
    let admitted_result = fixture.admitted()?;
    assert!(
        admitted_result.status.success(),
        "{}",
        String::from_utf8_lossy(&admitted_result.stderr)
    );
    let marker = fixture.root.path().join("ran-mismatch");
    let output = fixture.observe(&[], &["/usr/bin/touch", marker.to_str().ok_or("path")?])?;
    assert!(output.status.success());
    assert!(marker.exists());
    assert!(!fixture.state.join("observe.json").exists());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    Ok(())
}

/// Trace: FR-015-AC-15
#[test]
fn pid_only_catchable_signals_are_forwarded_and_observer_waits() -> TestResult {
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        let fixture = Fixture::new(1)?;
        let mut child = fixture
            .command()
            .arg("observe")
            .arg("--kind")
            .arg("test")
            .arg("--")
            .arg("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()?;
        wait_for_child(&fixture.state, child.id())?;
        nix::sys::signal::kill(Pid::from_raw(i32::try_from(child.id())?), signal)?;
        let status = child.wait()?;
        assert_eq!(status.signal(), Some(signal as i32), "{signal}");
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.state.join("observe.json"))?)?;
        assert_eq!(state["raw_statuses"][0]["signal"], signal as i32);
        let metrics = fixture.command().arg("metrics").output()?;
        assert!(
            String::from_utf8(metrics.stdout)?
                .contains("ix_build_test_runs_total{host=\"observe-test\",outcome=\"failed\"} 1")
        );
    }
    Ok(())
}

/// Trace: FR-015-AC-16
#[test]
fn dead_observer_keeps_live_command_busy_then_reclaims_without_outcome() -> TestResult {
    let fixture = Fixture::new(0)?;
    let mut observer = fixture
        .command()
        .arg("observe")
        .arg("--")
        .arg("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()?;
    let (command_pid, before) = wait_for_command_identity(&fixture.state, observer.id())?;
    nix::sys::signal::kill(
        Pid::from_raw(i32::try_from(observer.id())?),
        Signal::SIGKILL,
    )?;
    let _ = observer.wait()?;
    let metrics = fixture.command().arg("metrics").output()?;
    let text = String::from_utf8(metrics.stdout)?;
    assert!(text.contains("ix_build_slots_busy{host=\"observe-test\"} 1"));
    assert!(!text.contains("ix_build_builds_total"));
    assert!(!text.contains("ix_build_test_runs_total"));
    nix::sys::signal::kill(Pid::from_raw(i32::try_from(command_pid)?), Signal::SIGTERM)?;
    let _ = before;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let output = fixture.command().arg("metrics").output()?;
        if String::from_utf8(output.stdout)?
            .contains("ix_build_slots_busy{host=\"observe-test\"} 0")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "dead command slot was not reclaimed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// Trace: FR-015-AC-16
#[test]
fn second_observer_acquires_store_lock_while_first_command_runs() -> TestResult {
    let fixture = Fixture::new(1)?;
    let mut first = fixture
        .command()
        .arg("observe")
        .arg("--")
        .arg("/bin/sleep")
        .arg("2")
        .spawn()?;
    wait_for_command_identity(&fixture.state, first.id())?;
    assert!(fixture.observe(&[], &["/usr/bin/true"])?.status.success());
    let metrics = fixture.command().arg("metrics").output()?;
    assert!(
        String::from_utf8(metrics.stdout)?.contains("ix_build_slots_busy{host=\"observe-test\"} 1")
    );
    assert!(first.wait()?.success());
    Ok(())
}

/// Trace: FR-015-AC-16
#[test]
fn next_observe_reclaims_dead_observer_and_command_slot() -> TestResult {
    let fixture = Fixture::new(1)?;
    let mut observer = fixture
        .command()
        .arg("observe")
        .arg("--")
        .arg("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()?;
    let (command_pid, _) = wait_for_command_identity(&fixture.state, observer.id())?;
    nix::sys::signal::kill(
        Pid::from_raw(i32::try_from(observer.id())?),
        Signal::SIGKILL,
    )?;
    let _ = observer.wait()?;
    nix::sys::signal::kill(Pid::from_raw(i32::try_from(command_pid)?), Signal::SIGTERM)?;
    let output = fixture.observe(&[], &["/usr/bin/true"])?;
    assert!(output.status.success());
    let metrics = fixture.command().arg("metrics").output()?;
    assert!(
        String::from_utf8(metrics.stdout)?.contains("ix_build_slots_busy{host=\"observe-test\"} 0")
    );
    Ok(())
}

/// Trace: FR-015-AC-16
#[test]
fn reused_command_pid_is_not_counted_as_a_live_slot() -> TestResult {
    let fixture = Fixture::new(1)?;
    let mut observer = fixture
        .command()
        .arg("observe")
        .arg("--")
        .arg("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()?;
    let (command_pid, mut snapshot) = wait_for_command_identity(&fixture.state, observer.id())?;
    nix::sys::signal::kill(
        Pid::from_raw(i32::try_from(observer.id())?),
        Signal::SIGKILL,
    )?;
    let _ = observer.wait()?;
    let busy = snapshot["busy"]
        .as_object_mut()
        .ok_or("busy map")?
        .values_mut()
        .find(|row| row["command_pid"] == command_pid)
        .ok_or("busy row")?;
    busy["command_started"] = json!(busy["command_started"].as_u64().ok_or("start")? + 1);
    fs::write(
        fixture.state.join("observe.json"),
        serde_json::to_vec(&snapshot)?,
    )?;
    let metrics = fixture.command().arg("metrics").output()?;
    assert!(
        String::from_utf8(metrics.stdout)?.contains("ix_build_slots_busy{host=\"observe-test\"} 0")
    );
    nix::sys::signal::kill(Pid::from_raw(i32::try_from(command_pid)?), Signal::SIGTERM)?;
    Ok(())
}

/// Trace: FR-015-AC-17
#[test]
fn test_outcomes_and_raw_status_are_recorded_without_priority() -> TestResult {
    let fixture = Fixture::new(1)?;
    assert!(
        fixture
            .observe(&["--kind", "test"], &["/usr/bin/false"])?
            .status
            .code()
            == Some(1)
    );
    let metrics = fixture.command().arg("metrics").output()?;
    let text = String::from_utf8(metrics.stdout)?;
    assert!(text.contains("ix_build_test_runs_total{host=\"observe-test\",outcome=\"failed\"} 1"));
    assert!(!text.contains("ix_build_builds_total"));
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.state.join("observe.json"))?)?;
    assert_eq!(state["raw_statuses"][0]["exit_code"], 1);
    assert!(state["raw_statuses"][0]["signal"].is_null());
    Ok(())
}

/// Trace: FR-015-AC-18
#[test]
fn invalid_hints_and_unknown_options_warn_once_and_still_run() -> TestResult {
    let fixture = Fixture::new(1)?;
    let marker = fixture.root.path().join("ran-invalid-hints");
    let output = fixture
        .command()
        .arg("observe")
        .arg("--kind")
        .arg("lint")
        .arg("--priority")
        .arg("urgent")
        .arg("--queued-at-ms")
        .arg("abc")
        .arg("--future-option")
        .arg("--")
        .arg("/usr/bin/touch")
        .arg(&marker)
        .output()?;
    assert!(output.status.success());
    assert!(marker.exists());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    let metrics = fixture.command().arg("metrics").output()?;
    let text = String::from_utf8(metrics.stdout)?;
    assert!(!text.contains("ix_build_builds_total"));
    assert!(!text.contains("ix_build_test_runs_total"));
    assert!(!text.contains("ix_build_queue_wait_seconds"));
    Ok(())
}

/// Trace: FR-015-AC-18
#[test]
fn future_queue_timestamp_is_treated_as_absent_with_one_warning() -> TestResult {
    let fixture = Fixture::new(1)?;
    let output = fixture
        .command()
        .arg("observe")
        .arg("--priority")
        .arg("check")
        .arg("--queued-at-ms")
        .arg(u64::MAX.to_string())
        .arg("--")
        .arg("/usr/bin/true")
        .output()?;
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    let metrics = fixture.command().arg("metrics").output()?;
    assert!(!String::from_utf8(metrics.stdout)?.contains("ix_build_queue_wait_seconds"));
    Ok(())
}

/// Trace: FR-015-AC-19
#[test]
fn queue_wait_series_appear_only_for_priorities_with_observations() -> TestResult {
    let fixture = Fixture::new(1)?;
    assert!(
        fixture
            .observe(
                &["--priority", "gate", "--queued-at-ms", "1"],
                &["/usr/bin/true"]
            )?
            .status
            .success()
    );
    let metrics = fixture.command().arg("metrics").output()?;
    let text = String::from_utf8(metrics.stdout)?;
    assert!(
        text.contains("ix_build_queue_wait_seconds_bucket{host=\"observe-test\",priority=\"gate\"")
    );
    assert!(!text.contains("priority=\"check\",le"));
    Ok(())
}

/// Trace: FR-015-AC-20
#[test]
fn observed_json_omits_unknown_series_and_admitted_metrics_fallback_is_preserved() -> TestResult {
    let observed = Fixture::new(1)?;
    assert!(observed.observe(&[], &["/usr/bin/true"])?.status.success());
    let snapshot = observed.command().arg("status").output()?;
    assert!(snapshot.status.success());
    let json: serde_json::Value = serde_json::from_slice(&snapshot.stdout)?;
    assert!(json.get("ix_build_queue_depth").is_none());
    assert!(
        json.get("ix_build_admission_blocked_seconds_total")
            .is_none()
    );
    assert!(json.get("ix_build_pause_seconds_total").is_none());
    assert!(json.get("ix_build_paused_orphans").is_none());

    let admitted = Fixture::new(1)?;
    let admitted_result = admitted.admitted()?;
    assert!(
        admitted_result.status.success(),
        "{}",
        String::from_utf8_lossy(&admitted_result.stderr)
    );
    assert!(admitted.command().arg("metrics").output()?.status.success());
    assert!(admitted.command().arg("status").output()?.status.success());
    fs::remove_file(admitted.state.join("mode.json"))?;
    assert!(admitted.command().arg("metrics").output()?.status.success());
    assert!(admitted.command().arg("status").output()?.status.success());
    Ok(())
}

/// Trace: FR-015-AC-22
#[test]
fn run_and_prune_refuse_observed_store_without_changing_mode() -> TestResult {
    let fixture = Fixture::new(1)?;
    assert!(fixture.observe(&[], &["/usr/bin/true"])?.status.success());
    let before = fs::read(fixture.state.join("observe.json"))?;
    let run = fixture.admitted()?;
    assert!(!run.status.success());
    let prune = fixture.command().arg("prune").output()?;
    assert!(!prune.status.success());
    assert_eq!(fs::read(fixture.state.join("observe.json"))?, before);
    Ok(())
}

/// Trace: FR-015-AC-13
#[test]
fn observe_slot_is_unclamped_and_other_commands_do_not_signal_it() -> TestResult {
    let fixture = Fixture::new(0)?;
    let mut observer = fixture
        .command()
        .arg("observe")
        .arg("--")
        .arg("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()?;
    let (command_pid, _) = wait_for_command_identity(&fixture.state, observer.id())?;
    let metrics = fixture.command().arg("metrics").output()?;
    let text = String::from_utf8(metrics.stdout)?;
    assert!(text.contains("ix_build_slots_busy{host=\"observe-test\"} 1"));
    assert!(text.contains("ix_build_slots_configured{host=\"observe-test\"} 0"));
    for action in [
        vec![
            "run",
            "--owner",
            "test:team:lead",
            "--ticket",
            "SWM-242",
            "--",
            "/usr/bin/true",
        ],
        vec!["recover"],
        vec!["prune"],
        vec!["cancel", "not-a-request"],
    ] {
        assert!(!fixture.command().args(&action).output()?.status.success());
        assert!(
            nix::sys::signal::kill(Pid::from_raw(i32::try_from(command_pid)?), None).is_ok(),
            "command died after {action:?}"
        );
    }
    nix::sys::signal::killpg(
        Pid::from_raw(i32::try_from(observer.id())?),
        Signal::SIGTERM,
    )?;
    let _ = observer.wait()?;
    Ok(())
}

/// Trace: FR-015-AC-21
#[test]
fn post_start_lock_timeout_preserves_status_and_records_no_partial_outcome() -> TestResult {
    let fixture = Fixture::new(1)?;
    let observer = fixture
        .command()
        .arg("observe")
        .arg("--kind")
        .arg("build")
        .arg("--priority")
        .arg("check")
        .arg("--")
        .arg("/bin/sleep")
        .arg("0.3")
        .stderr(Stdio::piped())
        .spawn()?;
    wait_for_command_identity(&fixture.state, observer.id())?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.state.join("lock"))?;
    lock.lock()?;
    let started = std::time::Instant::now();
    let output = observer.wait_with_output()?;
    assert!(output.status.success());
    assert!(started.elapsed() < Duration::from_millis(1_500));
    drop(lock);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("observe warning:")
            .count(),
        1
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.state.join("observe.json"))?)?;
    assert_eq!(state["builds"], json!({}));
    assert_eq!(state["raw_statuses"], json!([]));
    Ok(())
}

/// Trace: FR-015-AC-22
#[test]
fn concurrent_first_observe_and_run_choose_one_store_mode() -> TestResult {
    let fixture = Fixture::new(1)?;
    let state = fixture.state.clone();
    let config = fixture.config.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let observe_barrier = barrier.clone();
    let observe_state = state.clone();
    let observe_config = config.clone();
    let observing = std::thread::spawn(move || {
        observe_barrier.wait();
        Command::new(env!("CARGO_BIN_EXE_locked-build"))
            .arg("--state")
            .arg(observe_state)
            .arg("--config")
            .arg(observe_config)
            .arg("observe")
            .arg("--")
            .arg("/bin/sleep")
            .arg("0.2")
            .output()
    });
    let run_barrier = barrier.clone();
    let running = std::thread::spawn(move || {
        run_barrier.wait();
        Command::new(env!("CARGO_BIN_EXE_locked-build"))
            .arg("--state")
            .arg(state)
            .arg("--config")
            .arg(config)
            .arg("run")
            .arg("--owner")
            .arg("test:team:lead")
            .arg("--ticket")
            .arg("SWM-242")
            .arg("--")
            .arg("/usr/bin/true")
            .output()
    });
    let _ = observing.join().map_err(|_| "observe thread")??;
    let _ = running.join().map_err(|_| "run thread")??;
    let observe_mode = fixture.state.join("observe.json").exists();
    let admitted_mode =
        fixture.state.join("mode.json").exists() || fixture.state.join("state.json").exists();
    assert_ne!(
        observe_mode, admitted_mode,
        "store selected mixed or no mode"
    );
    Ok(())
}

fn wait_for_child(state: &std::path::Path, observer: u32) -> TestResult {
    let _ = wait_for_command_identity(state, observer)?;
    Ok(())
}

fn wait_for_command_identity(
    state: &std::path::Path,
    observer: u32,
) -> Result<(u32, serde_json::Value), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(bytes) = fs::read(state.join("observe.json"))
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some((_, busy)) = value["busy"]
                .as_object()
                .and_then(|rows| rows.iter().find(|(_, row)| row["observer_pid"] == observer))
            && let Some(pid) = busy["command_pid"].as_u64()
        {
            return Ok((u32::try_from(pid)?, value));
        }
        assert!(
            std::time::Instant::now() < deadline,
            "observer did not persist child identity"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
