// SPDX-License-Identifier: MIT
//! Optional remote observations; unavailable remote data never invalidates local stats.
use super::{LIMIT, Ledger, command, emit, number, unavailable};
use crate::config::CachePolicy;
use crate::error::Result;
use serde_json::Value;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) fn backend(config: &Value) -> Option<&'static str> {
    let remote = config["values"]["cache"]["remote"].as_object()?;
    let kind = match remote.get("type") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => {
            let value = value.trim().to_ascii_lowercase();
            (!value.is_empty()).then_some(value)
        }
        _ => return None,
    };
    let nonempty = |key: &str| {
        remote
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    };
    match kind.as_deref() {
        Some("s3") => Some("s3"),
        Some("gcs") => Some("gcs"),
        Some("filesystem" | "fs") => Some("filesystem"),
        Some("oci") => Some("oci"),
        Some(_) => None,
        None => {
            if remote.get("repository").is_some_and(|v| !v.is_null())
                || remote.get("insecure").is_some_and(|v| !v.is_null())
            {
                return None;
            }
            let s3 = ["bucket", "endpoint", "region", "profile", "user_agent"]
                .into_iter()
                .any(nonempty);
            let filesystem = ["path", "atomic_write_dir"].into_iter().any(nonempty);
            match (s3, filesystem) {
                (true, true) | (false, false) => None,
                (true, false) => Some("s3"),
                (false, true) => Some("filesystem"),
            }
        }
    }
}

pub(super) fn inventory(stats: &Value, host: &str, text: &mut String) -> Result<()> {
    let mut stores = stats["stores"]
        .as_array()
        .ok_or_else(unavailable)?
        .iter()
        .collect::<Vec<_>>();
    stores.sort_by_key(|s| s["path"].as_str().unwrap_or_default());
    for (index, store) in stores.iter().take(4).enumerate() {
        let labels = format!(",cache=\"store_{index}\"");
        if let Some(bytes) = store["blob_stats"]["total_blob_size"].as_u64() {
            emit(
                text,
                "store_inventory_bytes",
                "gauge",
                host,
                "local",
                &labels,
                bytes,
            );
        }
        for (name, key) in [
            ("store_inventory_limit_bytes", "max_size"),
            ("store_inventory_entries", "entries"),
        ] {
            emit(
                text,
                name,
                "gauge",
                host,
                "local",
                &labels,
                number(store, key)?,
            );
        }
    }
    if stores.len() > 4 {
        emit(
            text,
            "store_inventory_truncated",
            "gauge",
            host,
            "local",
            "",
            stores.len() - 4,
        );
    }
    Ok(())
}

fn recent_json(path: &Path) -> Result<Value> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits() | nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() > 8 * LIMIT
        || metadata.modified()?.elapsed().unwrap_or(Duration::MAX) > Duration::from_secs(120)
    {
        return Err(unavailable());
    }
    Ok(serde_json::from_reader(file.take(8 * LIMIT))?)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observe(
    binary: &Path,
    stats: &Value,
    config: &Value,
    host: &str,
    ledger: &mut Ledger,
    text: &mut String,
    deadline: Instant,
    policy: &CachePolicy,
) {
    let Some(backend) = backend(config) else {
        return;
    };
    let labels = format!(",cache=\"remote\",backend=\"{backend}\"");
    emit(
        text,
        "remote_enabled",
        "gauge",
        host,
        "shared",
        &labels,
        u8::from(policy.shared_enabled()),
    );
    let reachable = policy
        .remote_status_file
        .as_deref()
        .and_then(|p| recent_json(p).ok())
        .and_then(|doctor| {
            if doctor["schema_version"] != 1 || doctor["command"] != "doctor" {
                return None;
            }
            let remote = doctor["checks"]
                .as_array()?
                .iter()
                .find(|c| c["label"] == "Remote")?;
            if remote["detail"]
                .as_str()
                .is_some_and(|s| s.contains("local-only"))
            {
                return None;
            }
            remote["pass"].as_bool().map(i32::from)
        })
        .unwrap_or(-1);
    emit(
        text,
        "remote_reachable",
        "gauge",
        host,
        "shared",
        &labels,
        if policy.shared_enabled() {
            reachable
        } else {
            -1
        },
    );
    if !policy.shared_enabled() {
        return;
    }
    let report = match &policy.remote_report_file {
        Some(path) => recent_json(path),
        None => command(
            binary,
            &[
                "stats", "--full", "--json", "--since", "1000000h", "--top", "1",
            ],
            deadline,
        ),
    };
    let mut working = ledger.clone();
    let mut observed = String::new();
    let available = report
        .and_then(|report| {
            render(
                &report,
                stats,
                host,
                &labels,
                backend,
                &mut working,
                &mut observed,
            )
        })
        .is_ok();
    if available {
        *ledger = working;
        text.push_str(&observed);
    }
    emit(
        text,
        "remote_observation_available",
        "gauge",
        host,
        "shared",
        &labels,
        u8::from(available),
    );
    emit(
        text,
        "local_fallback",
        "gauge",
        host,
        "local",
        "",
        u8::from(reachable == 0 && number(stats, "local_hits").unwrap_or(0) > 0),
    );
}

pub(super) fn render(
    report: &Value,
    stats: &Value,
    host: &str,
    labels: &str,
    backend: &str,
    ledger: &mut Ledger,
    text: &mut String,
) -> Result<()> {
    if report["schema_version"] != 1 || report["command"] != "stats" || report["success"] != true {
        return Err(unavailable());
    }
    let network = &report["network"];
    if !network.is_object() || network["configured_backend"].as_str() != Some(backend) {
        return Err(unavailable());
    }
    for (name, key) in [
        ("remote_push_total", "uploads_ok"),
        ("remote_pull_total", "downloads_ok"),
        ("remote_push_bytes_total", "bytes_up"),
        ("remote_pull_bytes_total", "bytes_down"),
        ("remote_push_errors_total", "uploads_failed"),
        ("remote_pull_errors_total", "downloads_failed"),
    ] {
        let raw = number(network, key)?;
        let epoch = stats["lifetime"]["since"]
            .as_str()
            .ok_or_else(unavailable)?;
        // Separate network baseline: the core renderer has already updated its epoch.
        let value = ledger.advance(
            name,
            raw,
            !ledger.remote_epoch.is_empty() && ledger.remote_epoch != epoch,
        )?;
        emit(text, name, "counter", host, "shared", labels, value);
    }
    ledger.remote_epoch = stats["lifetime"]["since"]
        .as_str()
        .ok_or_else(unavailable)?
        .to_owned();
    for (name, key) in [
        ("remote_pull_latency_mean_seconds", "avg_download_ms"),
        ("remote_pull_latency_p95_seconds", "p95_download_ms"),
    ] {
        let value = network[key]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.)
            .ok_or_else(unavailable)?;
        emit(text, name, "gauge", host, "shared", labels, value / 1000.);
    }
    Ok(())
}
