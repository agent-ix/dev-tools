// SPDX-License-Identifier: MIT
//! Shared persistent build admission and safe supervision (FR-005).
#![deny(unsafe_op_in_unsafe_fn)]

mod config;
mod error;
mod metrics;
mod model;
mod observe;
mod process;
mod runtime;
mod store;
mod watchdog;

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::error::Result;
use crate::model::RequestId;

#[derive(Parser)]
#[command(
    version,
    about = "Fair persistent build admission; host policy is required"
)]
struct Cli {
    #[arg(long, global = true)]
    state: Option<PathBuf>,
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// List stable error codes, or validate one code spelling.
    ErrorCodes { code: Option<String> },
    /// Enqueue once, supervise the command, and return its exact outcome.
    Run {
        #[arg(long)]
        owner: String,
        #[arg(long)]
        ticket: String,
        #[arg(long, default_value_t = 1)]
        units: u32,
        #[arg(long, default_value_t = 0)]
        priority: u32,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Print a redacted JSON snapshot of capacity, requests and transitions.
    Status,
    /// Cancel a queued or running request, preserving the lease through cleanup.
    Cancel { request: String },
    /// Recover durable queued requests and verify orphaned running groups.
    Recover,
    /// Remove terminal requests/events after the monitoring consumer saved them.
    Prune,
    /// Print Unix epoch milliseconds without opening the state store.
    NowMs,
    /// Export Prometheus text metrics to stdout or an atomic textfile-collector path.
    Metrics {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Run an already admitted command immediately while recording telemetry only.
    Observe {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        priority: Option<String>,
        #[arg(long)]
        queued_at_ms: Option<String>,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<OsString>,
    },
    #[command(hide = true)]
    Worker { request: String },
    #[command(hide = true)]
    Watchdog { request: String, generation: u64 },
    #[command(hide = true)]
    Parking,
}

fn execute(cli: Cli) -> Result<i32> {
    if let Action::Observe {
        kind,
        priority,
        queued_at_ms,
        command,
    } = &cli.command
    {
        return Ok(observe::run(
            cli.state.as_deref(),
            cli.config.as_deref(),
            kind.as_deref(),
            priority.as_deref(),
            queued_at_ms.as_deref(),
            command,
            false,
        ));
    }
    if matches!(cli.command, Action::Parking) {
        // EOF follows the watchdog's exit, including SIGKILL. No state access is needed.
        std::io::copy(&mut std::io::stdin(), &mut std::io::sink())?;
        return Ok(0);
    }
    if matches!(cli.command, Action::NowMs) {
        println!("{}", observe::now_ms());
        return Ok(0);
    }
    if let Action::ErrorCodes { code } = &cli.command {
        let codes = match code {
            Some(code) => vec![error::ErrorCode::from_code(code).ok_or_else(|| {
                error::Error::new(error::ErrorCode::Request, "unknown error code")
            })?],
            None => error::ErrorCode::all().to_vec(),
        };
        println!(
            "{}",
            serde_json::to_string(&codes.iter().map(|code| code.as_str()).collect::<Vec<_>>())?
        );
        return Ok(0);
    }
    if let Action::Metrics { output } = &cli.command
        && let (Some(state), Some(config)) = (cli.state.as_deref(), cli.config.as_deref())
    {
        let _export_lock = if output.is_some() {
            Some(store::lock_metrics_export_at(state).map_err(|message| {
                error::Error::new(error::ErrorCode::State, message.to_string())
            })?)
        } else {
            None
        };
        if let Some(text) = observe::exposition(state, config)? {
            if let Some(path) = output {
                metrics::write_atomic(path, &text)?;
            } else {
                print!("{text}");
            }
            return Ok(0);
        }
    }
    if matches!(&cli.command, Action::Status)
        && let Some(state) = cli.state.as_deref()
        && let Some(config) = cli.config.as_deref()
        && let Some(snapshot) = observe::snapshot(state, config)?
    {
        println!("{}", serde_json::to_string(&snapshot)?);
        return Ok(0);
    }
    let state = cli
        .state
        .as_deref()
        .ok_or_else(|| error::Error::new(error::ErrorCode::Configuration, "--state is required"))?;
    let config = cli.config.as_deref().ok_or_else(|| {
        error::Error::new(error::ErrorCode::Configuration, "--config is required")
    })?;
    let store = if matches!(&cli.command, Action::Metrics { .. }) {
        store::Store::open_existing(std::path::Path::new(state), std::path::Path::new(config))?
    } else {
        runtime::open(state, config)?
    };
    match cli.command {
        Action::ErrorCodes { .. } => Ok(0),
        Action::NowMs => Ok(0),
        Action::Parking => Ok(0),
        Action::Run {
            owner,
            ticket,
            units,
            priority,
            command,
        } => {
            let id = runtime::enqueue(&store, &owner, &ticket, units, priority, &command)?;
            eprintln!("request={} output={}", id.0, store.path.display());
            let worker = runtime::spawn_worker(&store, &id)?;
            runtime::wait(&store, &id, vec![worker])
        }
        Action::Status => {
            println!("{}", serde_json::to_string(&runtime::status(&store)?)?);
            Ok(0)
        }
        Action::Cancel { request } => {
            runtime::cancel(&store, &RequestId(request))?;
            Ok(0)
        }
        Action::Recover => {
            for id in runtime::recover(&store)? {
                runtime::spawn_worker(&store, &id)?;
            }
            println!("{}", serde_json::to_string(&runtime::status(&store)?)?);
            Ok(0)
        }
        Action::Prune => {
            println!("{}", runtime::prune(&store)?);
            Ok(0)
        }
        Action::Metrics { output } => {
            if let Some(output) = output {
                runtime::metrics_to_file(&store, &output)?;
            } else {
                print!("{}", runtime::metrics(&store)?);
            }
            Ok(0)
        }
        Action::Observe { .. } => unreachable!("observe handled before opening the store"),
        Action::Worker { request } => {
            runtime::worker(&store, &RequestId(request))?;
            Ok(0)
        }
        Action::Watchdog {
            request,
            generation,
        } => {
            watchdog::run(&store, &RequestId(request), generation)?;
            Ok(0)
        }
    }
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().collect();
    let parsed = Cli::try_parse_from(&args);
    let code = match parsed {
        Ok(cli) => match execute(cli) {
            Ok(code) => code,
            Err(error) => {
                observe::report_stderr(&format!("{}: {}\n", error.code.as_str(), error.message));
                2
            }
        },
        Err(error)
            if is_observe_command(&args)
                && !matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                ) =>
        {
            observe::run_parse_failure(&args)
        }
        Err(error) => {
            let message = error.to_string();
            let rendered = if message.ends_with('\n') {
                message
            } else {
                format!("{message}\n")
            };
            if error.use_stderr() {
                observe::report_stderr(&rendered);
            } else {
                let _ = std::io::stdout().lock().write_all(rendered.as_bytes());
            }
            error.exit_code()
        }
    };
    std::process::exit(code);
}

fn is_observe_command(args: &[OsString]) -> bool {
    let mut cursor = 1;
    while cursor < args.len() {
        match args[cursor].to_str() {
            Some("--") => return false,
            Some("--state" | "--config") => cursor += 2,
            Some("observe") => return true,
            Some(value) if value.starts_with('-') => cursor += 1,
            Some(_) | None => return false,
        }
    }
    false
}
