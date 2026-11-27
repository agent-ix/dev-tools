// SPDX-License-Identifier: MIT
//! Optional, bounded kache observations. No compiler or daemon mutation.
use crate::error::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LIMIT: u64 = 1_048_576;
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    epoch: String,
    gc_run: String,
    previous: BTreeMap<String, u64>,
    totals: BTreeMap<String, u64>,
}

fn unavailable() -> Error {
    Error::new(ErrorCode::State, "kache observation unavailable")
}

/// Optional override is an absolute executable path; absence on PATH emits nothing.
pub(crate) fn executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IX_BUILD_KACHE") {
        return Some(PathBuf::from(path));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        let path = dir.join("kache");
        path.metadata()
            .ok()
            .filter(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)?;
        path.canonicalize().ok()
    })
}

/// Appends fail-open telemetry; cache errors never fail scheduler collection.
pub(crate) fn append(text: &mut String, host: &str, ledger_path: &Path) {
    let binary = executable();
    append_with_binary(text, host, ledger_path, binary.as_deref());
}

pub(super) fn append_with_binary(
    text: &mut String,
    host: &str,
    ledger_path: &Path,
    binary: Option<&Path>,
) {
    append_for(
        text,
        host,
        ledger_path,
        binary,
        std::env::var_os("IX_BUILD_KACHE_EXPECT_DAEMON").as_deref()
            != Some(std::ffi::OsStr::new("0")),
    );
}

fn append_for(
    text: &mut String,
    host: &str,
    ledger_path: &Path,
    binary: Option<&Path>,
    expected: bool,
) {
    let Some(binary) = binary else { return };
    text.push_str(&format!("# HELP ix_kache_daemon_expected This host expects a kache daemon.\n# TYPE ix_kache_daemon_expected gauge\nix_kache_daemon_expected{{host=\"{}\"}} {}\n", crate::metrics::escape_label(host), u8::from(expected)));
    let mut known_store = "";
    let mut observed = String::new();
    match observe(binary, host, ledger_path, &mut observed, &mut known_store) {
        Ok(()) => text.push_str(&observed),
        Err(_) => {
            // Config may be unreadable: omit the store label until it is known.
            emit(text, "up", "gauge", host, known_store, "", 0);
            emit(text, "scrape_error", "gauge", host, known_store, "", 1);
        }
    }
}

fn command(binary: &Path, args: &[&str], deadline: Instant) -> Result<Value> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    // Only kache's location/configuration environment crosses this boundary.
    for key in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "KACHE_CONFIG",
        "KACHE_DIR",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let mut child = command.spawn()?;
    let mut reaped = false;
    let result = (|| {
        let mut stdout = child.stdout.take().ok_or_else(unavailable)?;
        nix::fcntl::fcntl(
            &stdout,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .map_err(|_| unavailable())?;
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            if Instant::now() >= deadline {
                return Err(unavailable());
            }
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    if let Some(status) = child.try_wait()? {
                        reaped = true;
                        if !status.success() {
                            return Err(unavailable());
                        }
                        let value: Value = serde_json::from_slice(&bytes)?;
                        if value["schema_version"] != 1 || value["success"] != true {
                            return Err(unavailable());
                        }
                        return Ok(value);
                    }
                }
                Ok(count) => {
                    bytes.extend_from_slice(&buffer[..count]);
                    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > LIMIT {
                        return Err(unavailable());
                    }
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    if result.is_err() && !reaped {
        // This child still owns its fresh process group, including any pipe holders.
        if let Ok(pid) = i32::try_from(child.id()) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    result
}

fn number(value: &Value, key: &str) -> Result<u64> {
    value[key].as_u64().ok_or_else(unavailable)
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().ok_or_else(unavailable)
}
fn timestamp(value: &str) -> Result<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|t| t.timestamp())
        .map_err(|_| unavailable())
}

fn observe(
    binary: &Path,
    host: &str,
    path: &Path,
    text: &mut String,
    known_store: &mut &'static str,
) -> Result<()> {
    if !binary.is_absolute() {
        return Err(unavailable());
    }
    let (_lock, mut ledger) = read_ledger(path)?;
    let deadline = Instant::now() + TIMEOUT;
    let config = command(binary, &["config", "list", "--json"], deadline)?;
    *known_store = store_label(&config["values"])?;
    let stats = command(
        binary,
        &["stats", "--json", "--since", "1000000h"],
        deadline,
    )?;
    let explain = command(binary, &["explain", "--all", "--json"], deadline)?;
    render(&stats, &config, &explain, host, &mut ledger, text)?;
    crate::metrics::write_atomic(path, &serde_json::to_string(&ledger)?)?;
    Ok(())
}

fn read_ledger(path: &Path) -> Result<(std::fs::File, Ledger)> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits() | nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path.with_extension("lock"))?;
    if !lock.metadata()?.is_file() {
        return Err(unavailable());
    }
    lock.try_lock().map_err(|_| unavailable())?;
    let ledger = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits() | nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)
    {
        Ok(file) => {
            if !file.metadata()?.is_file() || file.metadata()?.len() > LIMIT {
                return Err(unavailable());
            }
            serde_json::from_reader(file.take(LIMIT))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ledger::default(),
        Err(e) => return Err(e.into()),
    };
    Ok((lock, ledger))
}

// kache owns these extensible JSON envelopes. Read and type-check only verified
// fields, rather than inventing a parallel complete kache wire schema.
fn render(
    stats: &Value,
    config: &Value,
    explain: &Value,
    host: &str,
    ledger: &mut Ledger,
    text: &mut String,
) -> Result<()> {
    if stats["command"] != "stats"
        || config["command"] != "config"
        || explain["command"] != "explain"
    {
        return Err(unavailable());
    }
    let store = store_label(&config["values"])?;
    let epoch = string(&stats["lifetime"], "since")?;
    let epoch_timestamp = timestamp(epoch)?;
    let version = string(stats, "daemon_version")?;
    if version.is_empty() || version.len() > 64 {
        return Err(unavailable());
    }
    let connected = stats["daemon_connected"]
        .as_bool()
        .ok_or_else(unavailable)?;
    let mut entries = 0_u64;
    for source in stats["stores"].as_array().ok_or_else(unavailable)? {
        if !source["error"].is_null() {
            return Err(unavailable());
        }
        entries = entries
            .checked_add(number(source, "entries")?)
            .ok_or_else(unavailable)?;
    }
    emit(text, "up", "gauge", host, store, "", u8::from(connected));
    emit(text, "scrape_error", "gauge", host, store, "", 0);
    emit(
        text,
        "info",
        "gauge",
        host,
        store,
        &format!(",version=\"{}\"", crate::metrics::escape_label(version)),
        1,
    );
    emit(
        text,
        "counter_epoch",
        "gauge",
        host,
        store,
        "",
        epoch_timestamp,
    );
    emit(
        text,
        "store_bytes",
        "gauge",
        host,
        store,
        "",
        number(&stats["disk"], "store_bytes")?,
    );
    emit(
        text,
        "store_limit_bytes",
        "gauge",
        host,
        store,
        "",
        number(&stats["disk"], "store_limit_bytes")?,
    );
    emit(text, "entries", "gauge", host, store, "", entries);
    let new_epoch = ledger.epoch != epoch;
    for (name, raw, seconds) in [
        (
            "hits_total",
            number(stats, "local_hits")?
                .checked_add(number(stats, "readonly_hits")?)
                .and_then(|v| v.checked_add(stats["prefetch_hits"].as_u64()?))
                .and_then(|v| v.checked_add(stats["remote_hits"].as_u64()?))
                .ok_or_else(unavailable)?,
            false,
        ),
        ("misses_total", number(stats, "misses")?, false),
        ("remote_hits_total", number(stats, "remote_hits")?, false),
        (
            "hit_compile_seconds_total",
            number(&stats["lifetime"], "hit_compile_time_ms")?,
            true,
        ),
        (
            "miss_compile_seconds_total",
            number(stats, "miss_elapsed_ms")?,
            true,
        ),
        (
            "permit_wait_seconds_total",
            number(stats, "total_permit_wait_ms")?,
            true,
        ),
        (
            "hit_elapsed_seconds_total",
            number(stats, "hit_elapsed_ms")?,
            true,
        ),
    ] {
        let value = ledger.advance(name, raw, new_epoch)?;
        if seconds {
            emit(text, name, "counter", host, store, "", value as f64 / 1000.);
        } else {
            emit(text, name, "counter", host, store, "", value);
        }
    }
    let gc = &stats["gc"];
    if !gc.is_null() {
        let run = string(gc, "last_run")?;
        emit(
            text,
            "gc_last_run_timestamp",
            "gauge",
            host,
            store,
            "",
            timestamp(run)?,
        );
        if ledger.gc_run != run {
            ledger.add("gc_evicted_entries_total", number(gc, "entries_evicted")?)?;
            ledger.add("gc_evicted_bytes_total", number(gc, "bytes_freed")?)?;
            ledger.gc_run = run.into();
        }
    } else {
        emit(text, "gc_last_run_timestamp", "gauge", host, store, "", 0);
    }
    for name in ["gc_evicted_entries_total", "gc_evicted_bytes_total"] {
        emit(
            text,
            name,
            "counter",
            host,
            store,
            "",
            ledger.totals.get(name).copied().unwrap_or(0),
        );
    }
    let mut reasons: BTreeMap<&str, u64> = ["not_a_compile", "unsupported", "other"]
        .into_iter()
        .map(|r| (r, 0))
        .collect();
    for reason in explain["reasons"].as_array().ok_or_else(unavailable)? {
        if reason["result"] != "passthrough" {
            continue;
        }
        let raw = string(reason, "reason")?;
        let bucket = if raw.starts_with("not-a-compile|") {
            "not_a_compile"
        } else if raw.starts_with("unsupported|") {
            "unsupported"
        } else {
            "other"
        };
        let count = reasons.get_mut(bucket).ok_or_else(unavailable)?;
        *count = count
            .checked_add(number(reason, "count")?)
            .ok_or_else(unavailable)?;
    }
    // One family header regardless of reason count (Prometheus parser requirement).
    text.push_str("# HELP ix_kache_passthrough_total Observed retained-history passthroughs by bounded reason.\n# TYPE ix_kache_passthrough_total counter\n");
    for (reason, raw) in reasons {
        let count = ledger.advance(&format!("passthrough_{reason}"), raw, new_epoch)?;
        text.push_str(&format!("ix_kache_passthrough_total{{host=\"{}\",store=\"{store}\",reason=\"{reason}\"}} {count}\n", crate::metrics::escape_label(host)));
    }
    ledger.epoch = epoch.into();
    Ok(())
}

fn store_label(config: &Value) -> Result<&'static str> {
    if !config.is_object() {
        return Err(unavailable());
    }
    let local_only = match &config["cache"]["local_only"] {
        Value::Null => false,
        Value::Bool(v) => *v,
        _ => return Err(unavailable()),
    };
    Ok(
        if !local_only && config.get("remote").is_some_and(|r| !r.is_null()) {
            "shared"
        } else {
            "local"
        },
    )
}

impl Ledger {
    fn add(&mut self, name: &str, delta: u64) -> Result<u64> {
        let total = self.totals.entry(name.into()).or_default();
        *total = total.checked_add(delta).ok_or_else(unavailable)?;
        Ok(*total)
    }
    fn advance(&mut self, name: &str, raw: u64, new_epoch: bool) -> Result<u64> {
        let previous = self.previous.insert(name.into(), raw).unwrap_or(0);
        // A changed lifetime epoch or a truncated retained history starts a new
        // source baseline. Accumulated exporter totals remain monotonic.
        self.add(
            name,
            if new_epoch {
                raw
            } else {
                raw.saturating_sub(previous)
            },
        )
    }
}

fn emit(
    text: &mut String,
    suffix: &str,
    kind: &str,
    host: &str,
    store: &str,
    extra: &str,
    value: impl std::fmt::Display,
) {
    let name = format!("ix_kache_{suffix}");
    let store_label = if store.is_empty() {
        String::new()
    } else {
        format!(",store=\"{store}\"")
    };
    text.push_str(&format!("# HELP {name} Kache observed {suffix}.\n# TYPE {name} {kind}\n{name}{{host=\"{}\"{store_label}{extra}}} {value}\n", crate::metrics::escape_label(host)));
}

#[cfg(test)]
mod tests;
