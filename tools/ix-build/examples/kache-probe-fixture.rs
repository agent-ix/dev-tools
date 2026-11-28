// SPDX-License-Identifier: MIT
//! Process-boundary acceptance fixture: slow config followed by stalled stats.
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or("fixture executable has no parent")?;
    writeln!(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("probe-pids"))?,
        "{}",
        std::process::id()
    )?;
    let command = std::env::args().nth(1).ok_or("fixture command missing")?;
    let document = match command.as_str() {
        "config" => {
            std::thread::sleep(std::time::Duration::from_secs(4));
            include_str!("../src/sample/fixtures/luna-config.json")
        }
        "stats" => {
            std::thread::sleep(std::time::Duration::from_secs(30));
            include_str!("../src/sample/fixtures/luna-stats.json")
        }
        "explain" => include_str!("../src/sample/fixtures/luna-explain.json"),
        _ => return Err("unsupported fixture command".into()),
    };
    print!("{document}");
    Ok(())
}
