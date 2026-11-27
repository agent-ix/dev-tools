// SPDX-License-Identifier: MIT
//! Explicit, isolated fault lane. Each fault feeds the production alert rules.
use super::*;
use serde_json::json;
use std::process::{Child, Command, Stdio};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn draco_only() -> Result<std::ffi::OsString> {
    assert_eq!(
        sysinfo::System::host_name()
            .unwrap_or_default()
            .to_ascii_lowercase(),
        "draco",
        "fault lane is restricted to draco"
    );
    std::env::var_os("IX_BUILD_LOCKED_BUILD_BIN").ok_or_else(|| {
        Error::new(
            ErrorCode::Configuration,
            "IX_BUILD_LOCKED_BUILD_BIN must name the reviewed binary",
        )
    })
}
fn assert_fault_alert(text: &str, alert: &str) -> Result<()> {
    let root = tempfile::tempdir()?;
    let rules = root.path().join("alerts.yml");
    std::fs::write(&rules, include_str!("../../monitoring/alerts.yml"))?;
    let mut inputs = Vec::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        if let Some((series, value)) = line.rsplit_once(' ') {
            let value = if series.starts_with("ix_build_export_timestamp_seconds{") {
                "0"
            } else {
                value
            };
            inputs.push(json!({"series":series,"values":format!("{value}+0x30")}));
        }
    }
    let host = "your-build-host";
    inputs.push(json!({"series":format!("up{{host=\"{host}\",job=\"node\"}}"),"values":"1+0x30"}));
    let labels = if alert == "LockedBuildHostNoMetrics" {
        format!("host=\"{host}\",job=\"node\"")
    } else {
        format!("host=\"{host}\"")
    };
    let fixture = root.path().join("fault.test.yml");
    std::fs::write(
        &fixture,
        serde_json::to_vec_pretty(
            &json!({"rule_files":[rules],"evaluation_interval":"1m","tests":[{"name":alert,"interval":"1m","input_series":inputs,"promql_expr_test":[{"expr":format!("ALERTS{{alertname=\"{alert}\",alertstate=\"firing\"}}"),"eval_time":"20m","exp_samples":[{"labels":format!("ALERTS{{alertname=\"{alert}\",alertstate=\"firing\",severity=\"warning\",{labels}}}"),"value":1}]}]}]}),
        )?,
    )?;
    let promtool = std::env::var_os("IX_BUILD_PROMTOOL")
        .ok_or_else(|| Error::new(ErrorCode::Configuration, "IX_BUILD_PROMTOOL required"))?;
    let result = Command::new(promtool)
        .args(["test", "rules"])
        .arg(fixture)
        .output()?;
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}
fn cli_export(binary: &std::ffi::OsStr, store: &Store, output: &Path) -> Result<String> {
    let result = Command::new(binary)
        .arg("--state")
        .arg(&store.path)
        .arg("--config")
        .arg(&store.config_path)
        .args(["metrics", "--output"])
        .arg(output)
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(std::fs::read_to_string(output)?)
}

/// Trace: FR-015-AC-5, FR-015-AC-10
#[test]
#[ignore = "draco-only isolated fault lane; requires reviewed binary and promtool"]
fn telemetry_fault_killed_scheduler_fires_stuck_queue_alert() -> Result<()> {
    let binary = draco_only()?;
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    store.transaction(|state, _| {
        state.find_mut(&id)?.enqueued_clock_ms = clock_ms()?.saturating_sub(1_200_000);
        Ok(())
    })?;
    let spawn_lock = store
        .try_lock_member(&format!("{}.spawn.lock", id.0))?
        .ok_or_else(|| Error::new(ErrorCode::State, "fixture spawn lock busy"))?;
    let mut scheduler = OwnedChild(
        Command::new(binary)
            .arg("--state")
            .arg(&store.path)
            .arg("--config")
            .arg(&store.config_path)
            .arg("scheduler")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let started = std::time::Instant::now();
    loop {
        let elected = std::fs::read(store.path.join("scheduler.lock"))
            .ok()
            .is_some_and(|bytes| {
                bytes
                    .get(8..12)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_le_bytes)
                    == Some(scheduler.0.id())
            });
        if elected {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "scheduler did not acquire isolated election lock"
        );
        std::thread::yield_now();
    }
    scheduler.0.kill()?;
    scheduler.0.wait()?;
    let text = metrics(&store)?;
    assert_fault_alert(&text, "LockedBuildQueueStuck")?;
    cancel(&store, &id)?;
    drop(spawn_lock);
    Ok(())
}

/// Trace: FR-015-AC-15
#[test]
#[ignore = "draco-only isolated fault lane; requires reviewed binary and promtool"]
fn telemetry_fault_orphan_rustc_fires_unleased_process_alert() -> Result<()> {
    let _binary = draco_only()?;
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    cancel(&store, &id)?;
    let before = crate::sample::lease_health(&store.snapshot()?.0)
        .ok_or_else(|| Error::new(ErrorCode::State, "process probe unavailable"))?
        .1;
    // rustc waits for source on stdin, keeping a real compiler outside any lease.
    let mut compiler = OwnedChild(
        Command::new("rustc")
            .arg("-")
            .arg("--crate-name")
            .arg("swm417_orphan")
            .arg("-o")
            .arg(root.path().join("unused"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let started = std::time::Instant::now();
    loop {
        let after = crate::sample::lease_health(&store.snapshot()?.0)
            .ok_or_else(|| Error::new(ErrorCode::State, "process probe unavailable"))?
            .1;
        if after > before {
            break;
        }
        assert!(compiler.0.try_wait()?.is_none(), "rustc must remain alive");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "orphan rustc was not sampled"
        );
        std::thread::yield_now();
    }
    assert_fault_alert(&metrics(&store)?, "LockedBuildUnleasedProcess")?;
    Ok(())
}

/// Trace: FR-015-AC-5, FR-015-AC-10
#[test]
#[ignore = "draco-only isolated fault lane; requires reviewed binary and promtool"]
fn telemetry_fault_stopped_timer_and_unavailable_store_keep_previous_samples() -> Result<()> {
    let binary = draco_only()?;
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    cancel(&store, &id)?;
    let output = root.path().join("metrics.prom");
    let _ = cli_export(&binary, &store, &output)?;
    #[cfg(target_os = "macos")]
    {
        let label = format!("com.agent-ix.swm417-{}", crate::store::random_id()?);
        let target = format!("gui/{}/{}", nix::unistd::getuid(), label);
        let plist = root.path().join("timer.plist");
        let body = format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array><string>{}</string><string>--state</string><string>{}</string><string>--config</string><string>{}</string><string>metrics</string><string>--output</string><string>{}</string></array><key>RunAtLoad</key><true/><key>StartInterval</key><integer>1</integer></dict></plist>",
            Path::new(&binary).display(),
            store.path.display(),
            store.config_path.display(),
            output.display()
        );
        std::fs::write(&plist, body)?;
        let before = std::fs::metadata(&output)?.modified()?;
        let boot = Command::new("launchctl")
            .arg("bootstrap")
            .arg(format!("gui/{}", nix::unistd::getuid()))
            .arg(plist)
            .output()?;
        assert!(
            boot.status.success(),
            "{}",
            String::from_utf8_lossy(&boot.stderr)
        );
        let _guard = FaultTimer(target.clone());
        let deadline = std::time::Instant::now();
        while std::fs::metadata(&output)?.modified()? == before {
            assert!(
                deadline.elapsed() < Duration::from_secs(8),
                "fixture timer did not export"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            Command::new("launchctl")
                .args(["bootout", &target])
                .status()?
                .success()
        );
        assert!(
            !Command::new("launchctl")
                .args(["print", &target])
                .output()?
                .status
                .success()
        );
    }
    let previous = std::fs::read_to_string(&output)?;
    // Stop this fixture's refreshes; replay twenty minutes with the last textfile.
    assert_fault_alert(&previous, "LockedBuildExportStale")?;
    std::fs::rename(&store.path, root.path().join("unavailable-store"))?;
    let fallback = cli_export(&binary, &store, &output)?;
    for line in previous
        .lines()
        .filter(|l| !l.contains("ix_build_export_error"))
    {
        assert!(
            fallback.lines().any(|current| current == line),
            "lost previous sample {line}"
        );
    }
    assert!(fallback.contains("ix_build_export_error{host=\"your-build-host\"} 1"));
    assert_fault_alert(&fallback, "LockedBuildExportError")?;
    Ok(())
}

/// Trace: FR-015-AC-10
#[test]
#[ignore = "draco-only isolated fault lane; requires reviewed binary and promtool"]
fn telemetry_fault_absent_series_fires_while_node_remains_up() -> Result<()> {
    let binary = draco_only()?;
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    cancel(&store, &id)?;
    let output = root.path().join("metrics.prom");
    let _ = cli_export(&binary, &store, &output)?;
    std::fs::remove_file(&output)?;
    assert!(!output.exists());
    assert_fault_alert("", "LockedBuildHostNoMetrics")
}

#[cfg(target_os = "macos")]
struct FaultTimer(String);
#[cfg(target_os = "macos")]
impl Drop for FaultTimer {
    fn drop(&mut self) {
        let _ = Command::new("launchctl")
            .args(["bootout", &self.0])
            .output();
    }
}

#[cfg(target_os = "macos")]
struct MountedVolume(std::path::PathBuf);
#[cfg(target_os = "macos")]
impl Drop for MountedVolume {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil")
            .arg("detach")
            .arg(&self.0)
            .arg("-force")
            .output();
    }
}

/// Trace: FR-015-AC-10
#[test]
#[cfg(target_os = "macos")]
#[ignore = "draco-only isolated ENOSPC fault lane; mounts a disposable 32 MB image"]
fn telemetry_fault_full_state_directory_fires_free_space_alert() -> Result<()> {
    use std::io::Write;
    let _binary = draco_only()?;
    let root = tempfile::tempdir()?;
    let image = root.path().join("full.dmg");
    let mounted = root.path().join("volume");
    std::fs::create_dir(&mounted)?;
    let create = Command::new("hdiutil")
        .args([
            "create",
            "-size",
            "32m",
            "-fs",
            "HFS+",
            "-volname",
            "swm417-fault",
        ])
        .arg(&image)
        .output()?;
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let attach = Command::new("hdiutil")
        .arg("attach")
        .arg(&image)
        .arg("-nobrowse")
        .arg("-mountpoint")
        .arg(&mounted)
        .output()?;
    assert!(
        attach.status.success(),
        "{}",
        String::from_utf8_lossy(&attach.stderr)
    );
    let _volume = MountedVolume(mounted.clone());
    let store = store(&mounted)?;
    let id = queued(&store)?;
    cancel(&store, &id)?;
    let mut filler = File::create(mounted.join("fill"))?;
    let block = [0_u8; 65536];
    loop {
        match filler.write_all(&block) {
            Ok(()) => {}
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(nix::errno::Errno::ENOSPC as i32));
                break;
            }
        }
    }
    drop(filler);
    let text = metrics(&store)?;
    let free = metric_value(
        &text,
        "ix_build_state_dir_free_bytes{host=\"your-build-host\"} ",
    )
    .and_then(|n| n.parse::<f64>().ok())
    .ok_or_else(|| Error::new(ErrorCode::State, "state free metric missing"))?;
    assert!(free < 65536.);
    assert_fault_alert(&text, "LockedBuildStateDirFreeLow")
}

/// Trace: FR-015-AC-5, FR-015-AC-10
#[test]
#[ignore = "draco-only isolated FIFO/deadline regression lane; requires reviewed binary"]
fn telemetry_fault_blocked_store_keeps_previous_and_fifo_output_cannot_hang() -> Result<()> {
    let binary = draco_only()?;
    let root = tempfile::tempdir()?;
    let store = store(root.path())?;
    let id = queued(&store)?;
    cancel(&store, &id)?;
    let output = root.path().join("metrics.prom");
    let previous = cli_export(&binary, &store, &output)?;
    std::fs::remove_file(&store.config_path)?;
    let mode = nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR;
    nix::unistd::mkfifo(&store.config_path, mode)
        .map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
    let started = std::time::Instant::now();
    let fallback = cli_export(&binary, &store, &output)?;
    assert!(
        started.elapsed() < Duration::from_secs(9),
        "blocked-store fallback exceeded publisher deadline"
    );
    for line in previous
        .lines()
        .filter(|l| !l.contains("ix_build_export_error"))
    {
        assert!(
            fallback.lines().any(|actual| actual == line),
            "lost previous sample {line}"
        );
    }
    assert!(fallback.contains("ix_build_export_error{host=\"your-build-host\"} 1"));
    std::fs::rename(&store.path, root.path().join("unavailable-store"))?;
    std::fs::remove_file(&output)?;
    nix::unistd::mkfifo(&output, mode).map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
    let mut publisher = OwnedChild(
        Command::new(&binary)
            .arg("--state")
            .arg(&store.path)
            .arg("--config")
            .arg(&store.config_path)
            .args(["metrics", "--output"])
            .arg(&output)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = publisher.0.try_wait()? {
            assert!(!status.success(), "non-regular output must be refused");
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "FIFO output hung the publisher"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
