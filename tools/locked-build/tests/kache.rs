// SPDX-License-Identifier: MIT
//! Live, read-only cache acceptance and scheduled-exporter latency lane.
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// Provenance: SWM-416; run on luna with the real daemon and IX_BUILD_PROMTOOL.
#[test]
#[ignore = "live luna cache acceptance and latency; requires kache and promtool"]
fn kache_luna_exporter_latency() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let config = root.path().join("policy.json");
    let state = root.path().join("state");
    let output = root.path().join("metrics.prom");
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_locked-build"))
            .arg("--state")
            .arg(&state)
            .arg("--config")
            .arg(&config)
            .args(args)
            .env_remove("LOCKED_BUILD_LEASE")
            .env_remove("LOCKED_BUILD_STATE")
            .env_remove("LOCKED_BUILD_CONFIG")
            .env_remove("LOCKED_BUILD_OWNER")
            .env_remove("LOCKED_BUILD_TICKET")
            .output()
    };
    let initialized = invoke(&[
        "run",
        "--class",
        "point",
        "--ticket",
        "SWM-416",
        "--",
        "/usr/bin/true",
    ])?;
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let mut durations = Vec::new();
    let mut text = String::new();
    for _ in 0..40 {
        let start = Instant::now();
        let result = invoke(&[
            "metrics",
            "--output",
            output.to_str().ok_or("non-UTF8 output")?,
        ])?;
        durations.push(start.elapsed().as_secs_f64());
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        text = std::fs::read_to_string(&output)?;
        assert!(text.contains("ix_kache_up{host=\"luna\",store=\"local\"} 1"));
        assert!(text.contains("ix_build_export_error{host=\"luna\"} 0"));
        assert!(text.lines().filter(|l| l.starts_with("ix_kache_")).count() <= 60);
    }
    durations.sort_by(f64::total_cmp);
    let p95 = durations.get(37).copied().ok_or("missing p95")?;
    println!(
        "luna scheduled metrics with kache: n=40 min={:.6}s p50={:.6}s p95={p95:.6}s max={:.6}s",
        durations[0], durations[19], durations[39]
    );
    assert!(p95 < 0.5, "exporter p95={p95:.6}s exceeds 0.5s");
    let promtool = std::env::var_os("IX_BUILD_PROMTOOL").ok_or("IX_BUILD_PROMTOOL required")?;
    let checked = Command::new(promtool)
        .args(["check", "metrics"])
        .stdin(std::fs::File::open(&output)?)
        .output()?;
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    // Optional pilot publication verifies node_exporter ingestion, then removes it.
    if let Some(path) = std::env::var_os("IX_BUILD_KACHE_PILOT_TEXTFILE") {
        let path = Path::new(&path);
        assert!(!path.exists(), "refusing to replace an existing pilot file");
        let cache = text
            .lines()
            .filter(|l| {
                l.starts_with("ix_kache_")
                    || l.starts_with("# HELP ix_kache_")
                    || l.starts_with("# TYPE ix_kache_")
            })
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        std::fs::write(path, cache)?;
        let scrape = Command::new("curl")
            .args(["-fsS", "http://localhost:9100/metrics"])
            .output();
        std::fs::remove_file(path)?;
        let scrape = scrape?;
        assert!(scrape.status.success());
        let scrape = String::from_utf8(scrape.stdout)?;
        assert!(scrape.contains("ix_kache_up{host=\"luna\",store=\"local\"} 1"));
        println!(
            "luna node_exporter pilot: real ix_kache_up=1 ingested; temporary pilot file removed"
        );
    }
    Ok(())
}
