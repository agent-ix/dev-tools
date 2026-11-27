// SPDX-License-Identifier: MIT
//! Read-only health and compiler-wrapper contract fixture; never contacts a cache.
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let name = executable
        .file_name()
        .and_then(|p| p.to_str())
        .ok_or("fixture name")?;
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.first().is_some_and(|s| s == "--version") {
        if name.contains("hang") {
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        if name.contains("broken") {
            return Err("unhealthy fixture".into());
        }
        if name.contains("wrong") {
            println!("other 2.4.0");
            return Ok(());
        }
        println!("kache 2.4.0");
        return Ok(());
    }
    if arguments.first().is_some_and(|s| s == "--compiler") {
        println!(
            "{}",
            serde_json::json!({"compiled":true,"wrapper":std::env::var("RUSTC_WRAPPER").unwrap_or_default(),"local_only":std::env::var("KACHE_LOCAL_ONLY").unwrap_or_default(),"jobs":std::env::var("CARGO_BUILD_JOBS").unwrap_or_default(),"threads":std::env::var("RUST_TEST_THREADS").unwrap_or_default()})
        );
        return Ok(());
    }
    if name.contains("build") {
        let wrapper = std::env::var_os("RUSTC_WRAPPER").filter(|s| !s.is_empty());
        let mut compiler = match wrapper {
            Some(wrapper) => {
                let mut c = std::process::Command::new(wrapper);
                c.arg(&executable);
                c
            }
            None => std::process::Command::new(&executable),
        };
        let output = compiler.arg("--compiler").output()?;
        std::io::stdout().write_all(&output.stdout)?;
        if !output.status.success() {
            return Err("compiler fixture failed".into());
        }
        return Ok(());
    }
    // Remote-down mode exercises the wrapper contract's local-compile route.
    // It is not a substitute for a live kache remote-outage compiler pilot.
    if name.contains("remote-down") {
        eprintln!("fixture: remote unavailable, using local compiler");
    }
    let compiler = arguments.first().ok_or("compiler argument missing")?;
    let status = std::process::Command::new(compiler)
        .args(&arguments[1..])
        .status()?;
    if !status.success() {
        return Err("wrapped compiler failed".into());
    }
    Ok(())
}
