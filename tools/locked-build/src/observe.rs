// SPDX-License-Identifier: MIT
//! Bounded, fail-open observation for commands admitted by an external wrapper.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, RefreshKind, System};

use crate::config::Config;

const OBSERVE_LIMIT: u64 = 32 * 1024 * 1024;
const OBSERVER_BOUND: Duration = Duration::from_millis(1_000);

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObservationState {
    mode: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    slots: u32,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    busy: BTreeMap<String, Busy>,
    #[serde(default)]
    build_started: BTreeSet<String>,
    #[serde(default)]
    test_started: bool,
    #[serde(default)]
    builds: BTreeMap<String, BTreeMap<String, u64>>,
    #[serde(default)]
    tests: BTreeMap<String, u64>,
    #[serde(default)]
    queue_wait: BTreeMap<String, Vec<u64>>,
    #[serde(default)]
    raw_statuses: Vec<RawStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Busy {
    observer_pid: u32,
    observer_started: u64,
    command_pid: Option<u32>,
    command_started: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawStatus {
    exit_code: Option<i32>,
    signal: Option<i32>,
}

/// Parse a failed clap invocation as observe and run its command fail-open.
pub(crate) fn run_parse_failure(args: &[OsString]) -> i32 {
    let end = args
        .iter()
        .position(|value| value == "--")
        .unwrap_or(args.len());
    let Some(index) = args[..end].iter().position(|value| value == "observe") else {
        return 2;
    };
    let mut state = None;
    let mut config = None;
    let mut cursor = 1;
    while cursor < end {
        match args[cursor].to_str() {
            Some("--state") if cursor + 1 < end => {
                cursor += 1;
                state = Some(PathBuf::from(&args[cursor]));
            }
            Some("--config") if cursor + 1 < end => {
                cursor += 1;
                config = Some(PathBuf::from(&args[cursor]));
            }
            _ => {}
        }
        cursor += 1;
    }
    let mut kind = None;
    let mut priority = None;
    let mut queued = None;
    let bad = true;
    let mut delimiter = None;
    cursor = index + 1;
    while cursor < args.len() {
        if args[cursor] == "--" {
            delimiter = Some(cursor + 1);
            break;
        }
        match args[cursor].to_str() {
            Some("--kind") if cursor + 1 < args.len() => {
                cursor += 1;
                kind = args[cursor].to_str().map(str::to_owned);
            }
            Some("--priority") if cursor + 1 < args.len() => {
                cursor += 1;
                priority = args[cursor].to_str().map(str::to_owned);
            }
            Some("--queued-at-ms") if cursor + 1 < args.len() => {
                cursor += 1;
                queued = args[cursor].to_str().map(str::to_owned);
            }
            Some("--state") if cursor + 1 < args.len() => {
                cursor += 1;
                state = Some(PathBuf::from(&args[cursor]));
            }
            Some("--config") if cursor + 1 < args.len() => {
                cursor += 1;
                config = Some(PathBuf::from(&args[cursor]));
            }
            _ => {}
        }
        cursor += 1;
    }
    let command = delimiter.map(|at| args[at..].to_vec()).unwrap_or_default();
    run(
        state.as_deref(),
        config.as_deref(),
        kind.as_deref(),
        priority.as_deref(),
        queued.as_deref(),
        &command,
        bad,
    )
}

/// Execute an observed child without making admission or resource decisions.
pub(crate) fn run(
    state_path: Option<&Path>,
    config_path: Option<&Path>,
    kind_input: Option<&str>,
    priority_input: Option<&str>,
    queued_input: Option<&str>,
    command: &[OsString],
    parse_warning: bool,
) -> i32 {
    let Some(program) = command.first() else {
        warning("observe requires `-- <command...>`");
        return 2;
    };
    let mut warning_message =
        parse_warning.then(|| "unrecognized or malformed observe option".to_owned());
    let kind = match kind_input {
        None => None,
        Some("build") => Some("build"),
        Some("test") => Some("test"),
        Some(_) => {
            set_warning(
                &mut warning_message,
                "invalid --kind; treating it as absent",
            );
            None
        }
    };
    let priority = match priority_input {
        None => None,
        Some("check") => Some("check"),
        Some("test") => Some("test"),
        Some("gate") => Some("gate"),
        Some(_) => {
            set_warning(
                &mut warning_message,
                "invalid --priority; treating it as absent",
            );
            None
        }
    };
    let queued = match queued_input {
        None => None,
        Some(value) => match value.parse::<u64>() {
            Ok(value) => Some(value),
            Err(_) => {
                set_warning(
                    &mut warning_message,
                    "invalid --queued-at-ms; treating it as absent",
                );
                None
            }
        },
    };
    let invocation = Instant::now();
    let mut busy_id = None;
    let mut observer_state = None;
    match preflight(state_path, config_path, invocation + OBSERVER_BOUND) {
        Ok((root, id, config)) => {
            busy_id = Some(id);
            observer_state = Some((root, config));
        }
        Err(error) => set_warning(&mut warning_message, &error),
    }
    let mut warning_printed = false;
    print_warning(&warning_message, &mut warning_printed);

    let signal_flags = match SignalFlags::register() {
        Ok(flags) => Some(flags),
        Err(error) => {
            set_warning(
                &mut warning_message,
                &format!("signal observer unavailable: {error}"),
            );
            print_warning(&warning_message, &mut warning_printed);
            None
        }
    };
    let mut child = match Command::new(program).args(command.iter().skip(1)).spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("locked-build: observe could not start command: {error}");
            if let (Some((root, _)), Some(id)) = (&observer_state, busy_id.as_deref()) {
                let _ = update(root, Instant::now() + OBSERVER_BOUND, |state| {
                    state.busy.remove(id);
                    Ok(())
                });
            }
            return 127;
        }
    };
    let pid = child.id();
    let start_ms = now_ms();
    let child_started = wait_process_start(pid, invocation + OBSERVER_BOUND);
    let mut writes_ok = observer_state.is_some() && child_started.is_some();
    if let (Some((root, config)), Some(id), Some(started)) =
        (&observer_state, busy_id.as_deref(), child_started)
    {
        if let Err(error) = update(root, invocation + OBSERVER_BOUND, |state| {
            let row = state
                .busy
                .get_mut(id)
                .ok_or_else(|| "busy slot disappeared".to_owned())?;
            row.command_pid = Some(pid);
            row.command_started = Some(started);
            if kind == Some("build")
                && let Some(priority) = priority
            {
                state.build_started.insert(priority.to_owned());
            }
            if kind == Some("test") {
                state.test_started = true;
            }
            if let (Some(queued), Some(priority)) =
                (queued.filter(|value| *value <= start_ms), priority)
            {
                state
                    .queue_wait
                    .entry(priority.to_owned())
                    .or_default()
                    .push(start_ms.saturating_sub(queued));
            }
            state.host = config.host.clone();
            state.slots = config.capacity_units;
            Ok(())
        }) {
            set_warning(&mut warning_message, &error);
            writes_ok = false;
        }
    } else if observer_state.is_some() {
        set_warning(
            &mut warning_message,
            "could not verify observed command identity",
        );
        writes_ok = false;
    }
    if queued.is_some_and(|value| value > start_ms) {
        set_warning(
            &mut warning_message,
            "future --queued-at-ms; treating it as absent",
        );
    }
    print_warning(&warning_message, &mut warning_printed);

    let mut received_signal = None;
    let (status, status_known) = 'wait: loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status, true),
            Ok(None) => {}
            Err(error) => {
                set_warning(&mut warning_message, &format!("wait failed: {error}"));
                loop {
                    match child.wait() {
                        Ok(status) => break 'wait (status, true),
                        Err(_)
                            if nix::sys::signal::kill(
                                nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap_or(i32::MAX)),
                                None,
                            )
                            .is_err() =>
                        {
                            break 'wait (ExitStatus::from_raw(1 << 8), false);
                        }
                        Err(_) => thread::sleep(Duration::from_millis(10)),
                    }
                }
            }
        }
        if let Some(flags) = &signal_flags {
            for (signal, flag) in &flags.items {
                if flag.swap(false, Ordering::SeqCst) {
                    received_signal.get_or_insert(*signal);
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap_or(i32::MAX)),
                        *signal,
                    );
                }
            }
        }
        thread::sleep(Duration::from_millis(5));
    };
    if let (Some((root, _)), Some(id)) = (&observer_state, busy_id.as_deref()) {
        let deadline = Instant::now() + OBSERVER_BOUND;
        if let Err(error) = update(root, deadline, |state| {
            state.busy.remove(id);
            if writes_ok && status_known {
                let outcome = if status.success() { "ok" } else { "failed" };
                if kind == Some("build")
                    && let Some(priority) = priority
                {
                    *state
                        .builds
                        .entry(priority.to_owned())
                        .or_default()
                        .entry(outcome.to_owned())
                        .or_default() += 1;
                }
                if kind == Some("test") {
                    *state.tests.entry(outcome.to_owned()).or_default() += 1;
                }
                state.raw_statuses.push(RawStatus {
                    exit_code: status.code(),
                    signal: status.signal(),
                });
                if state.raw_statuses.len() > 10_000 {
                    state.raw_statuses.remove(0);
                }
            }
            Ok(())
        }) {
            set_warning(&mut warning_message, &error);
        }
    }
    print_warning(&warning_message, &mut warning_printed);
    if let Some(signal) = received_signal.or_else(|| {
        status
            .signal()
            .and_then(|number| nix::sys::signal::Signal::try_from(number).ok())
    }) {
        let _ = signal_hook::low_level::emulate_default_handler(signal as i32);
    }
    status.code().unwrap_or_else(|| {
        status
            .signal()
            .map_or(1, |signal| 128_i32.saturating_add(signal))
    })
}

struct SignalFlags {
    items: Vec<(nix::sys::signal::Signal, Arc<AtomicBool>)>,
}

impl SignalFlags {
    fn register() -> Result<Self, String> {
        let mut items = Vec::new();
        for signal in [
            nix::sys::signal::Signal::SIGHUP,
            nix::sys::signal::Signal::SIGINT,
            nix::sys::signal::Signal::SIGQUIT,
            nix::sys::signal::Signal::SIGALRM,
            nix::sys::signal::Signal::SIGPIPE,
            nix::sys::signal::Signal::SIGTERM,
            nix::sys::signal::Signal::SIGUSR1,
            nix::sys::signal::Signal::SIGUSR2,
            nix::sys::signal::Signal::SIGTSTP,
            nix::sys::signal::Signal::SIGCONT,
            nix::sys::signal::Signal::SIGTTIN,
            nix::sys::signal::Signal::SIGTTOU,
            nix::sys::signal::Signal::SIGURG,
            nix::sys::signal::Signal::SIGXCPU,
            nix::sys::signal::Signal::SIGXFSZ,
            nix::sys::signal::Signal::SIGVTALRM,
            nix::sys::signal::Signal::SIGPROF,
            nix::sys::signal::Signal::SIGWINCH,
            nix::sys::signal::Signal::SIGIO,
        ] {
            let flag = Arc::new(AtomicBool::new(false));
            signal_hook::flag::register(signal as i32, flag.clone())
                .map_err(|error| error.to_string())?;
            items.push((signal, flag));
        }
        Ok(Self { items })
    }
}

fn preflight(
    state: Option<&Path>,
    config_path: Option<&Path>,
    deadline: Instant,
) -> Result<(PathBuf, String, Config), String> {
    let root = state.ok_or_else(|| "--state is required".to_owned())?;
    let config_path = config_path.ok_or_else(|| "--config is required".to_owned())?;
    let config_meta = fs::symlink_metadata(config_path).map_err(|error| error.to_string())?;
    if !config_meta.is_file() || config_meta.len() > 1_048_576 {
        return Err("config must be a bounded regular file".into());
    }
    let config_path = config_path.to_path_buf();
    let config_wait = deadline.saturating_duration_since(Instant::now());
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(Config::read(&config_path).map_err(|error| error.to_string()));
    });
    let config = receiver
        .recv_timeout(config_wait)
        .map_err(|_| "config read exceeded 1000 ms".to_owned())??;
    let metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("state directory missing, unsafe, or not private".into());
    }
    let observer_start = bounded_process_start(std::process::id(), deadline)
        .ok_or_else(|| "cannot verify observer identity within 1000 ms".to_owned())?;
    let mut id = String::new();
    update(root, deadline, |state| {
        reclaim(state);
        state.host = config.host.clone();
        state.slots = config.capacity_units;
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| "observation id exhausted".to_owned())?;
        id = format!("{}:{}", std::process::id(), state.next_id);
        state.busy.insert(
            id.clone(),
            Busy {
                observer_pid: std::process::id(),
                observer_started: observer_start,
                command_pid: None,
                command_started: None,
            },
        );
        Ok(())
    })?;
    if Instant::now() > deadline {
        return Err("observer pre-start budget expired".into());
    }
    Ok((root.to_path_buf(), id, config))
}

fn update(
    root: &Path,
    deadline: Instant,
    operation: impl FnOnce(&mut ObservationState) -> Result<(), String>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(root.join("lock")).ok();
    if metadata.is_some_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink()) {
        return Err("state lock is not a regular file".into());
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(root.join("lock"))
        .map_err(|error| error.to_string())?;
    acquire(&lock, deadline)?;
    if root.join("mode.json").exists() {
        let mode: String = serde_json::from_slice(&bounded_read(&root.join("mode.json"))?)
            .map_err(|error| error.to_string())?;
        if mode != "observed" {
            return Err("store is in admitted mode".into());
        }
    }
    if root.join("state.json").exists() {
        return Err("store is in admitted mode".into());
    }
    let path = root.join("observe.json");
    let mut state = if path.exists() {
        serde_json::from_slice(&bounded_read(&path)?)
            .map_err(|error| format!("corrupt observed state: {error}"))?
    } else {
        ObservationState::default()
    };
    if !state.mode.is_empty() && state.mode != "observed" {
        return Err("store mode mismatch".into());
    }
    operation(&mut state)?;
    state.mode = "observed".to_owned();
    if Instant::now() > deadline {
        return Err("observer I/O deadline expired".into());
    }
    write_state(&path, &state, deadline)
}

fn acquire(lock: &File, deadline: Instant) -> Result<(), String> {
    loop {
        if Instant::now() >= deadline {
            return Err("state lock timeout".into());
        }
        match lock.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(2)),
            Err(error) => return Err(format!("state lock unavailable: {error}")),
        }
    }
}

fn bounded_read(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > OBSERVE_LIMIT {
        return Err("invalid or oversized observer state".into());
    }
    fs::read(path).map_err(|error| error.to_string())
}

fn write_state(path: &Path, state: &ObservationState, deadline: Instant) -> Result<(), String> {
    let temporary = path.with_extension("json.next");
    let bytes = serde_json::to_vec(state).map_err(|error| error.to_string())?;
    let result = (|| {
        if Instant::now() >= deadline {
            return Err("observer I/O deadline expired".to_owned());
        }
        match fs::symlink_metadata(&temporary) {
            Ok(metadata) if metadata.is_file() => {
                fs::remove_file(&temporary).map_err(|error| error.to_string())?
            }
            Ok(_) => return Err("observer temporary path is not a regular file".to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        if Instant::now() >= deadline {
            return Err("observer I/O deadline expired".to_owned());
        }
        fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Render observed-store metrics, pruning only slots whose observer and command identities are gone.
pub(crate) fn exposition(state_path: &Path) -> Result<Option<String>, String> {
    if !observed_mode(state_path)? {
        return Ok(None);
    }
    let deadline = Instant::now() + OBSERVER_BOUND;
    let mut result = None;
    update(state_path, deadline, |state| {
        reclaim(state);
        result = Some(render(state));
        Ok(())
    })?;
    Ok(result)
}

/// Return the observed-store JSON snapshot without scheduler-only values.
pub(crate) fn snapshot(state_path: &Path) -> Result<Option<serde_json::Value>, String> {
    if !observed_mode(state_path)? {
        return Ok(None);
    }
    let deadline = Instant::now() + OBSERVER_BOUND;
    let mut result = None;
    update(state_path, deadline, |state| {
        reclaim(state);
        let mut snapshot = serde_json::Map::new();
        snapshot.insert("mode".into(), serde_json::json!("observed"));
        snapshot.insert("host".into(), serde_json::json!(state.host));
        snapshot.insert(
            "ix_build_slots_busy".into(),
            serde_json::json!(state.busy.len()),
        );
        snapshot.insert(
            "ix_build_slots_configured".into(),
            serde_json::json!(state.slots),
        );
        if !state.build_started.is_empty() {
            snapshot.insert(
                "ix_build_builds_total".into(),
                serde_json::json!(state.builds),
            );
        }
        if state.test_started {
            snapshot.insert(
                "ix_build_test_runs_total".into(),
                serde_json::json!(state.tests),
            );
        }
        if state.queue_wait.values().any(|waits| !waits.is_empty()) {
            snapshot.insert(
                "ix_build_queue_wait_seconds".into(),
                serde_json::json!(state.queue_wait),
            );
        }
        result = Some(serde_json::Value::Object(snapshot));
        Ok(())
    })?;
    Ok(result)
}

fn observed_mode(root: &Path) -> Result<bool, String> {
    let path = root.join("observe.json");
    if !path.exists() {
        return Ok(false);
    }
    let state: ObservationState = serde_json::from_slice(&bounded_read(&path)?)
        .map_err(|error| format!("corrupt observed state: {error}"))?;
    Ok(state.mode == "observed")
}

fn reclaim(state: &mut ObservationState) {
    let pids: Vec<_> = state
        .busy
        .values()
        .flat_map(|busy| [Some(busy.observer_pid), busy.command_pid])
        .flatten()
        .map(sysinfo::Pid::from_u32)
        .collect();
    let (sender, receiver) = std::sync::mpsc::channel();
    let _worker = thread::spawn(move || {
        let mut system = System::new_with_specifics(RefreshKind::nothing());
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::everything(),
        );
        let alive = system
            .processes()
            .iter()
            .filter(|(_, process)| process.status() != ProcessStatus::Zombie)
            .map(|(pid, process)| (pid.as_u32(), process.start_time()))
            .collect::<BTreeMap<_, _>>();
        let _ = sender.send(alive);
    });
    let Ok(alive) = receiver.recv_timeout(Duration::from_millis(250)) else {
        return;
    };
    state.busy.retain(|_, busy| {
        let observer_alive = alive.get(&busy.observer_pid) == Some(&busy.observer_started);
        let command_alive = busy
            .command_pid
            .zip(busy.command_started)
            .is_some_and(|(pid, start)| alive.get(&pid) == Some(&start));
        observer_alive || command_alive
    });
}

fn render(state: &ObservationState) -> String {
    let host = escape(&state.host);
    let mut output = format!(
        "# HELP ix_build_slots_busy Currently observed command slots.\n# TYPE ix_build_slots_busy gauge\nix_build_slots_busy{{host=\"{host}\"}} {}\n# HELP ix_build_slots_configured Configured slots from the host policy.\n# TYPE ix_build_slots_configured gauge\nix_build_slots_configured{{host=\"{host}\"}} {}\n",
        state.busy.len(),
        state.slots
    );
    if !state.build_started.is_empty() {
        output.push_str("# HELP ix_build_builds_total Observed build outcomes by priority and result.\n# TYPE ix_build_builds_total counter\n");
    }
    for priority in &state.build_started {
        for outcome in ["ok", "failed"] {
            let count = state
                .builds
                .get(priority)
                .and_then(|values| values.get(outcome))
                .copied()
                .unwrap_or(0);
            output.push_str(&format!("ix_build_builds_total{{host=\"{host}\",priority=\"{}\",outcome=\"{outcome}\"}} {count}\n", escape(priority)));
        }
    }
    if state.test_started {
        output.push_str("# HELP ix_build_test_runs_total Observed test-run outcomes.\n# TYPE ix_build_test_runs_total counter\n");
        for outcome in ["ok", "failed"] {
            let count = state.tests.get(outcome).copied().unwrap_or(0);
            output.push_str(&format!(
                "ix_build_test_runs_total{{host=\"{host}\",outcome=\"{outcome}\"}} {count}\n"
            ));
        }
    }
    if state.queue_wait.values().any(|waits| !waits.is_empty()) {
        output.push_str("# HELP ix_build_queue_wait_seconds Observed wrapper queue wait.\n# TYPE ix_build_queue_wait_seconds histogram\n");
    }
    for (priority, waits) in &state.queue_wait {
        if waits.is_empty() {
            continue;
        }
        let sum: u64 = waits.iter().copied().sum();
        for bound in [
            1_000_u64, 5_000, 10_000, 30_000, 60_000, 120_000, 300_000, 600_000, 1_200_000,
            1_800_000, 3_600_000, 7_200_000,
        ] {
            let count = waits.iter().filter(|wait| **wait <= bound).count();
            output.push_str(&format!("ix_build_queue_wait_seconds_bucket{{host=\"{host}\",priority=\"{}\",le=\"{}\"}} {count}\n", escape(priority), bound / 1000));
        }
        output.push_str(&format!("ix_build_queue_wait_seconds_bucket{{host=\"{host}\",priority=\"{}\",le=\"+Inf\"}} {}\nix_build_queue_wait_seconds_sum{{host=\"{host}\",priority=\"{}\"}} {}\nix_build_queue_wait_seconds_count{{host=\"{host}\",priority=\"{}\"}} {}\n", escape(priority), waits.len(), escape(priority), sum as f64 / 1000.0, escape(priority), waits.len()));
    }
    output
}

fn wait_process_start(pid: u32, deadline: Instant) -> Option<u64> {
    while Instant::now() < deadline {
        if let Some(start) = process_start(pid) {
            return Some(start);
        }
        thread::sleep(Duration::from_millis(5));
    }
    None
}

fn process_start(pid: u32) -> Option<u64> {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = System::new_with_specifics(RefreshKind::nothing());
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::everything(),
    );
    system.process(pid).map(sysinfo::Process::start_time)
}

fn bounded_process_start(pid: u32, deadline: Instant) -> Option<u64> {
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(process_start(pid));
    });
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
        .flatten()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
        })
}
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}
fn set_warning(warning: &mut Option<String>, message: &str) {
    if warning.is_none() {
        *warning = Some(message.to_owned());
    }
}
fn print_warning(message: &Option<String>, printed: &mut bool) {
    if !*printed && let Some(message) = message {
        warning(message);
        *printed = true;
    }
}
fn warning(message: &str) {
    eprintln!("locked-build: observe warning: {message}");
}
