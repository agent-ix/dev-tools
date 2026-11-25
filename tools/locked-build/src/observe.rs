// SPDX-License-Identifier: MIT
//! Bounded, fail-open observation for commands admitted by an external wrapper.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
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
use crate::error::{Error, ErrorCode, Result as AppResult};

const OBSERVE_LIMIT: u64 = 32 * 1024 * 1024;
const OBSERVER_BOUND: Duration = Duration::from_millis(1_000);

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObservationState {
    #[serde(default)]
    schema_version: u32,
    mode: String,
    #[serde(default)]
    observed_at_ms: u64,
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
    queue_wait: BTreeMap<String, crate::metrics::Histogram>,
    #[serde(default)]
    raw_statuses: Vec<RawStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Busy {
    observer_pid: u32,
    #[serde(default)]
    observer_started: Option<u64>,
    command_pid: Option<u32>,
    command_started: Option<u64>,
}

struct ObserveHints {
    kind: Option<String>,
    priority: Option<String>,
    queued_at_ms: Option<u64>,
    started_at_ms: u64,
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
            Some("--kind") if args.get(cursor + 1).is_some_and(|value| value != "--") => {
                cursor += 1;
                kind = args[cursor].to_str().map(str::to_owned);
            }
            Some("--priority") if args.get(cursor + 1).is_some_and(|value| value != "--") => {
                cursor += 1;
                priority = args[cursor].to_str().map(str::to_owned);
            }
            Some("--queued-at-ms") if args.get(cursor + 1).is_some_and(|value| value != "--") => {
                cursor += 1;
                queued = args[cursor].to_str().map(str::to_owned);
            }
            Some("--state") if args.get(cursor + 1).is_some_and(|value| value != "--") => {
                cursor += 1;
                state = Some(PathBuf::from(&args[cursor]));
            }
            Some("--config") if args.get(cursor + 1).is_some_and(|value| value != "--") => {
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
#[allow(unsafe_code)]
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
    // Start the build before touching observer state. Observation is best effort and must
    // never hold up the already-admitted command.
    #[cfg(target_os = "linux")]
    let sigchld_error = SignalFlags::register_sigchld().err();
    let mut command_builder = Command::new(program);
    command_builder.args(command.iter().skip(1));
    // Rust ignores SIGPIPE in its runtime; Command resets it in the child. Restore that
    // inherited disposition so observed children match their caller's behavior.
    // SAFETY: The closure runs in the post-fork child and performs only sigaction(2), a
    // signal-safe operation; it does not access shared Rust state.
    unsafe {
        command_builder.pre_exec(|| {
            let action = nix::sys::signal::SigAction::new(
                nix::sys::signal::SigHandler::SigIgn,
                nix::sys::signal::SaFlags::empty(),
                nix::sys::signal::SigSet::empty(),
            );
            nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGPIPE, &action)
                .map(|_| ())
                .map_err(|error| std::io::Error::from_raw_os_error(error as i32))
        });
    }
    let mut child = match command_builder.spawn() {
        Ok(child) => child,
        Err(error) => {
            report_stderr(&format!(
                "locked-build: observe could not start command: {error}\n"
            ));
            return if error.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            };
        }
    };
    let mut warning_printed = false;
    #[cfg(target_os = "linux")]
    if let Some(error) = sigchld_error {
        set_warning(
            &mut warning_message,
            &format!("SIGCHLD observer unavailable: {error}"),
        );
        print_warning(&warning_message, &mut warning_printed);
    }
    // Register only after spawn so the child inherits the caller's dispositions. Stop
    // signals retain their normal job-control behavior; both processes stop together.
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
    let pid = child.id();
    let start_ms = now_ms();
    let cancellation = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = std::sync::mpsc::channel();
    let (started_sender, started_receiver) = std::sync::mpsc::channel();
    let state_path_owned = state_path.map(Path::to_path_buf);
    let config_path_owned = config_path.map(Path::to_path_buf);
    let hints = ObserveHints {
        kind: kind.map(str::to_owned),
        priority: priority.map(str::to_owned),
        queued_at_ms: queued,
        started_at_ms: start_ms,
    };
    let worker_cancellation = Arc::clone(&cancellation);
    let _preflight_worker = thread::spawn(move || {
        let deadline = Instant::now() + OBSERVER_BOUND;
        let _ = started_sender.send(deadline);
        let result = preflight(
            state_path_owned.as_deref(),
            config_path_owned.as_deref(),
            deadline,
            pid,
            worker_cancellation,
            hints,
        );
        let _ = sender.send(result);
    });
    let mut preflight_result = None;
    let mut preflight_cancelled = false;
    let mut preflight_deadline = None;
    let mut writes_ok = false;
    if queued.is_some_and(|value| value > start_ms) {
        set_warning(
            &mut warning_message,
            "future --queued-at-ms; treating it as absent",
        );
    }
    print_warning(&warning_message, &mut warning_printed);

    let (status, status_known) = 'wait: loop {
        if preflight_result.is_none() {
            if preflight_deadline.is_none() {
                preflight_deadline = started_receiver.try_recv().ok();
            }
            match receiver.try_recv() {
                Ok(result) => preflight_result = Some(result),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    preflight_result = Some(Err("observer preflight worker stopped".to_owned()));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            if preflight_result.is_none()
                && preflight_deadline.is_some_and(|deadline| Instant::now() >= deadline)
                && !preflight_cancelled
            {
                cancellation.store(true, Ordering::SeqCst);
                preflight_cancelled = true;
                set_warning(&mut warning_message, "observer preflight exceeded 1000 ms");
                print_warning(&warning_message, &mut warning_printed);
            }
        }
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
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap_or(i32::MAX)),
                        *signal,
                    );
                }
            }
        }
        thread::sleep(Duration::from_millis(5));
    };
    if preflight_result.is_none() {
        let deadline = preflight_deadline
            .or_else(|| started_receiver.recv_timeout(OBSERVER_BOUND).ok())
            .unwrap_or_else(Instant::now);
        let remaining = deadline.saturating_duration_since(Instant::now());
        preflight_result = Some(match receiver.recv_timeout(remaining) {
            Ok(result) => result,
            Err(_) => {
                cancellation.store(true, Ordering::SeqCst);
                Err("observer I/O exceeded 1000 ms".to_owned())
            }
        });
    }
    let preflight_result = preflight_result
        .unwrap_or_else(|| Err("observer preflight did not return within 1000 ms".to_owned()));
    let (observer_state, busy_id) = match preflight_result {
        Ok((root, id, config)) => {
            writes_ok = true;
            (Some((root, config)), Some(id))
        }
        Err(error) => {
            set_warning(&mut warning_message, &error);
            (None, None)
        }
    };
    print_warning(&warning_message, &mut warning_printed);
    if let (Some((root, _)), Some(id)) = (&observer_state, busy_id.as_deref()) {
        let root = root.clone();
        let id = id.to_owned();
        let kind = kind.map(str::to_owned);
        let priority = priority.map(str::to_owned);
        let (sender, receiver) = std::sync::mpsc::channel();
        let _completion_worker = thread::spawn(move || {
            let cancellation = AtomicBool::new(false);
            let result = update(
                &root,
                Instant::now() + OBSERVER_BOUND,
                &cancellation,
                |state| {
                    state.busy.remove(&id);
                    if writes_ok && status_known {
                        let outcome = if status.success() { "ok" } else { "failed" };
                        if kind.as_deref() == Some("build")
                            && let Some(priority) = priority.as_deref()
                        {
                            *state
                                .builds
                                .entry(priority.to_owned())
                                .or_default()
                                .entry(outcome.to_owned())
                                .or_default() += 1;
                        }
                        if kind.as_deref() == Some("test") {
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
                },
            );
            let _ = sender.send(result);
        });
        match receiver.recv_timeout(OBSERVER_BOUND) {
            Ok(Err(error)) => set_warning(&mut warning_message, &error.to_string()),
            Err(_) => set_warning(
                &mut warning_message,
                "observer completion write exceeded 1000 ms",
            ),
            Ok(Ok(())) => {}
        }
    }
    print_warning(&warning_message, &mut warning_printed);
    if let Some(signal) = status
        .signal()
        .and_then(|number| nix::sys::signal::Signal::try_from(number).ok())
    {
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
    #[cfg(target_os = "linux")]
    fn register_sigchld() -> Result<(), String> {
        let flag = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(nix::sys::signal::Signal::SIGCHLD as i32, flag)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

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
            nix::sys::signal::Signal::SIGCONT,
            nix::sys::signal::Signal::SIGURG,
            nix::sys::signal::Signal::SIGXCPU,
            nix::sys::signal::Signal::SIGXFSZ,
            nix::sys::signal::Signal::SIGABRT,
            nix::sys::signal::Signal::SIGTRAP,
            nix::sys::signal::Signal::SIGBUS,
            nix::sys::signal::Signal::SIGSYS,
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
    command_pid: u32,
    cancellation: Arc<AtomicBool>,
    hints: ObserveHints,
) -> Result<(PathBuf, String, Config), String> {
    let root = state.ok_or_else(|| "--state is required".to_owned())?;
    let config_path = config_path.ok_or_else(|| "--config is required".to_owned())?;
    let config_meta = fs::metadata(config_path).map_err(|error| error.to_string())?;
    if !config_meta.is_file() || config_meta.len() > 1_048_576 {
        return Err("config must be a bounded regular file".into());
    }
    let config = Config::read(config_path).map_err(|error| error.to_string())?;
    let root = crate::store::validate_state_directory(root).map_err(|error| error.to_string())?;
    let command_started = process_start(command_pid);
    let mut id = String::new();
    update(&root, deadline, &cancellation, |state| {
        if !state.busy.is_empty() {
            reclaim(state);
        }
        state.host = config.host.clone();
        state.slots = config.capacity_units;
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorCode::State, "observation id exhausted"))?;
        id = format!("{}:{}", std::process::id(), state.next_id);
        state.busy.insert(
            id.clone(),
            Busy {
                observer_pid: std::process::id(),
                observer_started: None,
                command_pid: Some(command_pid),
                command_started,
            },
        );
        if hints.kind.as_deref() == Some("build")
            && let Some(priority) = hints.priority.as_deref()
        {
            state.build_started.insert(priority.to_owned());
        }
        if hints.kind.as_deref() == Some("test") {
            state.test_started = true;
        }
        if let (Some(queued), Some(priority)) = (
            hints
                .queued_at_ms
                .filter(|value| *value <= hints.started_at_ms),
            hints.priority.as_deref(),
        ) {
            state
                .queue_wait
                .entry(priority.to_owned())
                .or_default()
                .observe_ms(hints.started_at_ms.saturating_sub(queued))?;
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    Ok((root, id, config))
}

fn update(
    root: &Path,
    deadline: Instant,
    cancellation: &AtomicBool,
    operation: impl FnOnce(&mut ObservationState) -> AppResult<()>,
) -> AppResult<()> {
    if cancellation.load(Ordering::SeqCst) {
        return Err(Error::new(ErrorCode::Io, "observer update cancelled"));
    }
    let metadata = fs::symlink_metadata(root.join("lock")).ok();
    if metadata.is_some_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink()) {
        return Err(Error::new(
            ErrorCode::State,
            "state lock is not a regular file",
        ));
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(root.join("lock"))?;
    acquire(&lock, deadline, cancellation)?;
    if root.join("mode.json").exists() {
        let mode: String = serde_json::from_slice(&bounded_read(&root.join("mode.json"))?)?;
        if mode != "observed" {
            return Err(Error::new(ErrorCode::Request, "store is in admitted mode"));
        }
    }
    if root.join("state.json").exists() {
        return Err(Error::new(ErrorCode::Request, "store is in admitted mode"));
    }
    let path = root.join("observe.json");
    let mut state = if path.exists() {
        serde_json::from_slice(&bounded_read(&path)?)?
    } else {
        ObservationState::default()
    };
    if !state.mode.is_empty() && state.mode != "observed" {
        return Err(Error::new(ErrorCode::Request, "store mode mismatch"));
    }
    operation(&mut state)?;
    state.schema_version = 1;
    state.mode = "observed".to_owned();
    state.observed_at_ms = now_ms();
    if cancellation.load(Ordering::SeqCst) || Instant::now() > deadline {
        return Err(Error::new(ErrorCode::Io, "observer I/O deadline expired"));
    }
    write_state(&path, &state, deadline, cancellation)
}

fn acquire(lock: &File, deadline: Instant, cancellation: &AtomicBool) -> AppResult<()> {
    loop {
        if cancellation.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorCode::Io, "state lock wait cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(Error::new(ErrorCode::Io, "state lock timeout"));
        }
        match lock.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(2)),
            Err(error) => {
                return Err(Error::new(
                    ErrorCode::Io,
                    format!("state lock unavailable: {error}"),
                ));
            }
        }
    }
}

fn bounded_read(path: &Path) -> AppResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > OBSERVE_LIMIT {
        return Err(Error::new(
            ErrorCode::State,
            "invalid or oversized observer state",
        ));
    }
    fs::read(path).map_err(Into::into)
}

fn write_state(
    path: &Path,
    state: &ObservationState,
    deadline: Instant,
    cancellation: &AtomicBool,
) -> AppResult<()> {
    let temporary = path.with_extension("json.next");
    let bytes = serde_json::to_vec(state)?;
    if cancellation.load(Ordering::SeqCst) || Instant::now() >= deadline {
        return Err(Error::new(ErrorCode::Io, "observer I/O deadline expired"));
    }
    crate::store::write_atomic_path(path, &temporary, &bytes, 0o600, Some(cancellation))
}

/// Render observed-store metrics, pruning only slots whose observer and command identities are gone.
pub(crate) fn exposition(state_path: &Path, config_path: &Path) -> AppResult<Option<String>> {
    if !observed_mode(state_path)? {
        return Ok(None);
    }
    let config = Config::read(config_path)?;
    let deadline = Instant::now() + OBSERVER_BOUND;
    let mut result = None;
    let cancellation = AtomicBool::new(false);
    let update_result = update(state_path, deadline, &cancellation, |state| {
        reclaim(state);
        result = Some(state.clone());
        Ok(())
    });
    if update_result.is_err() && result.is_none() {
        result = Some(read_observed_state(state_path)?);
    }
    Ok(result.map(|state| render(&state, &config.host, config.capacity_units)))
}

/// Return the observed-store JSON snapshot without scheduler-only values.
pub(crate) fn snapshot(
    state_path: &Path,
    config_path: &Path,
) -> AppResult<Option<serde_json::Value>> {
    if !observed_mode(state_path)? {
        return Ok(None);
    }
    let config = Config::read(config_path)?;
    let deadline = Instant::now() + OBSERVER_BOUND;
    let mut result = None;
    let cancellation = AtomicBool::new(false);
    let update_result = update(state_path, deadline, &cancellation, |state| {
        reclaim(state);
        result = Some(state.clone());
        Ok(())
    });
    if update_result.is_err() && result.is_none() {
        result = Some(read_observed_state(state_path)?);
    }
    Ok(result.map(|state| snapshot_json(&state, &config.host, config.capacity_units)))
}

fn observed_mode(root: &Path) -> AppResult<bool> {
    let path = root.join("observe.json");
    if !path.exists() {
        return Ok(false);
    }
    let state: ObservationState = serde_json::from_slice(&bounded_read(&path)?)?;
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
        let observer_alive = busy.observer_started.map_or_else(
            || alive.contains_key(&busy.observer_pid),
            |start| alive.get(&busy.observer_pid) == Some(&start),
        );
        let command_alive = busy.command_pid.is_some_and(|pid| {
            busy.command_started.map_or_else(
                || alive.contains_key(&pid),
                |start| alive.get(&pid) == Some(&start),
            )
        });
        observer_alive || command_alive
    });
}

fn render(state: &ObservationState, host: &str, slots: u32) -> String {
    let host = crate::metrics::escape_label(host);
    let mut output = format!(
        "# HELP ix_build_slots_busy Currently observed command slots.\n# TYPE ix_build_slots_busy gauge\nix_build_slots_busy{{host=\"{host}\"}} {}\n# HELP ix_build_slots_configured Configured slots from the host policy.\n# TYPE ix_build_slots_configured gauge\nix_build_slots_configured{{host=\"{host}\"}} {}\n",
        state.busy.len(),
        slots
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
            output.push_str(&format!("ix_build_builds_total{{host=\"{host}\",priority=\"{}\",outcome=\"{outcome}\"}} {count}\n", crate::metrics::escape_label(priority)));
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
    if state
        .queue_wait
        .values()
        .any(|histogram| histogram.count > 0)
    {
        output.push_str("# HELP ix_build_queue_wait_seconds Observed wrapper queue wait.\n# TYPE ix_build_queue_wait_seconds histogram\n");
    }
    for (priority, histogram) in &state.queue_wait {
        if histogram.count == 0 {
            continue;
        }
        for bound in crate::metrics::BUCKETS_SECONDS {
            let count = histogram.buckets.get(&bound).copied().unwrap_or(0);
            output.push_str(&format!("ix_build_queue_wait_seconds_bucket{{host=\"{host}\",priority=\"{}\",le=\"{bound}\"}} {count}\n", crate::metrics::escape_label(priority)));
        }
        output.push_str(&format!("ix_build_queue_wait_seconds_bucket{{host=\"{host}\",priority=\"{}\",le=\"+Inf\"}} {}\nix_build_queue_wait_seconds_sum{{host=\"{host}\",priority=\"{}\"}} {}\nix_build_queue_wait_seconds_count{{host=\"{host}\",priority=\"{}\"}} {}\n", crate::metrics::escape_label(priority), histogram.count, crate::metrics::escape_label(priority), crate::metrics::seconds(histogram.sum_ms), crate::metrics::escape_label(priority), histogram.count));
    }
    output
}

fn snapshot_json(state: &ObservationState, host: &str, slots: u32) -> serde_json::Value {
    let mut snapshot = serde_json::Map::new();
    snapshot.insert("schema_version".into(), serde_json::json!(1));
    snapshot.insert("observed_at_ms".into(), serde_json::json!(now_ms()));
    snapshot.insert("mode".into(), serde_json::json!("observed"));
    snapshot.insert("host".into(), serde_json::json!(host));
    snapshot.insert(
        "ix_build_slots_busy".into(),
        serde_json::json!(state.busy.len()),
    );
    snapshot.insert("ix_build_slots_configured".into(), serde_json::json!(slots));
    if !state.build_started.is_empty() {
        let builds = state
            .build_started
            .iter()
            .map(|priority| {
                let outcomes = state.builds.get(priority);
                (
                    priority.clone(),
                    serde_json::json!({
                        "ok": outcomes.and_then(|values| values.get("ok")).copied().unwrap_or(0),
                        "failed": outcomes.and_then(|values| values.get("failed")).copied().unwrap_or(0),
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        snapshot.insert(
            "ix_build_builds_total".into(),
            serde_json::Value::Object(builds),
        );
    }
    if state.test_started {
        snapshot.insert(
            "ix_build_test_runs_total".into(),
            serde_json::json!({
                "ok": state.tests.get("ok").copied().unwrap_or(0),
                "failed": state.tests.get("failed").copied().unwrap_or(0),
            }),
        );
    }
    if state
        .queue_wait
        .values()
        .any(|histogram| histogram.count > 0)
    {
        snapshot.insert(
            "ix_build_queue_wait_seconds".into(),
            serde_json::json!(state.queue_wait),
        );
    }
    serde_json::Value::Object(snapshot)
}

fn read_observed_state(root: &Path) -> AppResult<ObservationState> {
    let bytes = bounded_read(&root.join("observe.json"))?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
        })
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
    report_stderr(&format!("locked-build: observe warning: {message}\n"));
}

pub(crate) fn report_stderr(message: &str) {
    let _ = std::io::stderr().lock().write_all(message.as_bytes());
}
