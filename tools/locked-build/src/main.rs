// SPDX-License-Identifier: MIT
//! Shared persistent build admission and safe supervision (FR-005).
#![forbid(unsafe_code)]

mod config;
mod error;
mod metrics;
mod model;
mod process;
mod runtime;
mod sample;
mod store;
mod watchdog;

use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use clap::{Parser, Subcommand};

use crate::error::Result;
use crate::model::RequestId;

const DEFAULT_CARGO_CLASS: &str = "point";
const DEFAULT_CARGO_OWNER: &str = "cargo-shim";
const DEFAULT_CARGO_TICKET: &str = "MANUAL";

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
        #[arg(long, conflicts_with = "class")]
        priority: Option<u32>,
        #[arg(long, conflicts_with = "priority")]
        class: Option<String>,
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
    /// Export Prometheus text metrics to stdout or an atomic textfile-collector path.
    Metrics {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Sample configured read-only host sources into Prometheus text.
    Sample {
        #[arg(long)]
        output: PathBuf,
    },
    #[command(hide = true)]
    Worker { request: String },
    #[command(hide = true)]
    Watchdog { request: String, generation: u64 },
    #[command(hide = true)]
    Parking,
}

fn execute(cli: Cli) -> Result<i32> {
    if let Action::Sample { output } = &cli.command {
        let config = cli.config.as_deref().ok_or_else(|| {
            error::Error::new(error::ErrorCode::Configuration, "--config is required")
        })?;
        return sample::run(config, output);
    }
    if matches!(cli.command, Action::Parking) {
        // EOF follows the watchdog's exit, including SIGKILL. No state access is needed.
        std::io::copy(&mut std::io::stdin(), &mut std::io::sink())?;
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
    let (state, config) = resolve_paths(cli.state.as_deref(), cli.config.as_deref(), |name| {
        std::env::var_os(name).map(PathBuf::from)
    })?;
    let store = if matches!(&cli.command, Action::Metrics { .. }) {
        store::Store::open_existing(&state, &config)?
    } else {
        runtime::open(&state, &config)?
    };
    match cli.command {
        Action::ErrorCodes { .. } => Ok(0),
        Action::Parking => Ok(0),
        Action::Run {
            owner,
            ticket,
            units,
            priority,
            class,
            command,
        } => {
            let class_name = class.as_deref().unwrap_or("periodic");
            let priority = if class.is_some() {
                runtime::priority_for_class(&store, class_name)?
            } else if let Some(priority) = priority {
                priority
            } else {
                runtime::priority_for_class(&store, class_name)?
            };
            let id = runtime::enqueue(
                &store,
                &owner,
                &ticket,
                units,
                priority,
                Some(class_name),
                &command,
            )?;
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
        Action::Sample { .. } => unreachable!("sample handled before opening scheduler state"),
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

fn resolve_paths(
    state_flag: Option<&std::path::Path>,
    config_flag: Option<&std::path::Path>,
    mut env: impl FnMut(&str) -> Option<PathBuf>,
) -> Result<(PathBuf, PathBuf)> {
    let state = state_flag
        .map(PathBuf::from)
        .or_else(|| env("LOCKED_BUILD_STATE"));
    let config = config_flag
        .map(PathBuf::from)
        .or_else(|| env("LOCKED_BUILD_CONFIG"));
    if let (Some(state), Some(config)) = (&state, &config) {
        return Ok((state.clone(), config.clone()));
    }
    let home = env("HOME")
        .ok_or_else(|| error::Error::new(error::ErrorCode::Configuration, "HOME is not set"))?;
    let state = state.unwrap_or_else(|| {
        env("XDG_STATE_HOME")
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".local/state"))
            .join("locked-build")
    });
    let config = config.unwrap_or_else(|| {
        env("XDG_CONFIG_HOME")
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config"))
            .join("locked-build/policy.json")
    });
    Ok((state, config))
}

fn is_cargo_invocation(arguments: &[OsString]) -> bool {
    arguments
        .first()
        .and_then(|argument| PathBuf::from(argument).file_name().map(OsStr::to_owned))
        .is_some_and(|name| name == "cargo")
}

struct CargoShimInvocation<'a> {
    arguments: &'a [OsString],
    real_cargo: &'a std::path::Path,
    config_path: &'a std::path::Path,
    state_path: &'a std::path::Path,
    class: &'a str,
    owner: &'a str,
    ticket: &'a str,
    in_lease: bool,
}

fn cargo_shim_command(invocation: CargoShimInvocation<'_>) -> Result<Command> {
    let CargoShimInvocation {
        arguments,
        real_cargo,
        config_path,
        state_path,
        class,
        owner,
        ticket,
        in_lease,
    } = invocation;
    let real_cargo_path = real_cargo;
    let real_cargo_resolved = real_cargo_path.canonicalize()?;
    let executable = std::env::current_exe()?.canonicalize()?;
    if real_cargo_resolved == executable {
        return Err(error::Error::new(
            error::ErrorCode::Configuration,
            "real_cargo resolves to locked-build",
        ));
    }
    if in_lease {
        let mut command = Command::new(real_cargo_path);
        command.arg0("cargo");
        command.args(arguments.iter().skip(1));
        return Ok(command);
    }
    let mut command = Command::new(executable);
    command
        .arg("--state")
        .arg(state_path)
        .arg("--config")
        .arg(config_path)
        .arg("run")
        .arg("--owner")
        .arg(owner)
        .arg("--ticket")
        .arg(ticket)
        .arg("--class")
        .arg(class)
        .arg("--")
        .arg(real_cargo_path)
        .args(arguments.iter().skip(1));
    Ok(command)
}

fn cargo_shim(arguments: &[OsString]) -> Result<i32> {
    let (state_path, config_path) =
        resolve_paths(None, None, |name| std::env::var_os(name).map(PathBuf::from))?;
    let config = config::Config::read(&config_path)?;
    let real_cargo = config.real_cargo.as_deref().ok_or_else(|| {
        error::Error::new(
            error::ErrorCode::Configuration,
            "real_cargo is not configured",
        )
    })?;
    let class = std::env::var("LOCKED_BUILD_CLASS").unwrap_or_else(|_| DEFAULT_CARGO_CLASS.into());
    let owner = std::env::var("LOCKED_BUILD_OWNER").unwrap_or_else(|_| DEFAULT_CARGO_OWNER.into());
    let ticket =
        std::env::var("LOCKED_BUILD_TICKET").unwrap_or_else(|_| DEFAULT_CARGO_TICKET.into());
    let mut command = cargo_shim_command(CargoShimInvocation {
        arguments,
        real_cargo,
        config_path: &config_path,
        state_path: &state_path,
        class: &class,
        owner: &owner,
        ticket: &ticket,
        in_lease: std::env::var_os("LOCKED_BUILD_LEASE").is_some(),
    })?;
    let error = command.exec();
    Err(error.into())
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().collect();
    if is_cargo_invocation(&arguments) {
        let code = match cargo_shim(&arguments) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("{}: {}", error.code.as_str(), error.message);
                2
            }
        };
        std::process::exit(code);
    }
    let code = match execute(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{}: {}", error.code.as_str(), error.message);
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_accepts_class_and_rejects_class_with_numeric_priority() {
        let parsed = Cli::try_parse_from([
            "locked-build",
            "run",
            "--owner",
            "draco:team:code",
            "--ticket",
            "TEAM-1",
            "--class",
            "must-fix",
            "--",
            "cargo",
            "test",
        ]);
        assert!(
            matches!(parsed, Ok(Cli { command: Action::Run { class: Some(class), priority: None, .. }, .. }) if class == "must-fix")
        );

        let conflict = Cli::try_parse_from([
            "locked-build",
            "run",
            "--owner",
            "draco:team:code",
            "--ticket",
            "TEAM-1",
            "--priority",
            "1",
            "--class",
            "point",
            "--",
            "cargo",
            "test",
        ]);
        assert!(conflict.is_err());
    }

    #[test]
    fn cargo_shim_executes_real_cargo_directly_inside_a_lease() -> Result<()> {
        let arguments = ["cargo", "test", "-p", "locked-build"]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        assert!(is_cargo_invocation(&arguments));
        let root = tempfile::tempdir()?;
        let real_cargo = root.path().join("cargo");
        std::os::unix::fs::symlink("/bin/echo", &real_cargo)?;
        let command = cargo_shim_command(CargoShimInvocation {
            arguments: &arguments,
            real_cargo: &real_cargo,
            config_path: std::path::Path::new("/policy.json"),
            state_path: std::path::Path::new("/state"),
            class: DEFAULT_CARGO_CLASS,
            owner: DEFAULT_CARGO_OWNER,
            ticket: DEFAULT_CARGO_TICKET,
            in_lease: true,
        })?;
        assert_eq!(command.get_program(), real_cargo.as_os_str());
        let actual: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(actual, ["test", "-p", "locked-build"]);
        Ok(())
    }

    #[test]
    fn run_and_shim_paths_default_from_home_or_xdg_and_honor_precedence() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        std::fs::create_dir_all(&home)?;
        let vars = [
            ("HOME", home.clone()),
            ("XDG_STATE_HOME", root.path().join("xdg-state")),
            ("XDG_CONFIG_HOME", root.path().join("xdg-config")),
        ];
        let (state, config) = resolve_paths(None, None, |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        })?;
        assert_eq!(state, root.path().join("xdg-state/locked-build"));
        assert_eq!(
            config,
            root.path().join("xdg-config/locked-build/policy.json")
        );
        let (home_state, home_config) = resolve_paths(None, None, |name| match name {
            "HOME" => Some(home.clone()),
            "XDG_STATE_HOME" => Some("relative/state".into()),
            "XDG_CONFIG_HOME" => Some("relative/config".into()),
            _ => None,
        })?;
        assert_eq!(home_state, home.join(".local/state/locked-build"));
        assert_eq!(home_config, home.join(".config/locked-build/policy.json"));

        let policy = root.path().join("policy.json");
        std::fs::write(&policy, include_str!("../policy.example.json"))?;
        let store = runtime::open(&state, &policy)?;
        assert_eq!(store.path, state.canonicalize()?);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&state)?.permissions().mode() & 0o777,
            0o700
        );

        let state_flag = root.path().join("flag-state");
        let config_flag = root.path().join("flag-policy");
        let (resolved_state, resolved_config) =
            resolve_paths(Some(&state_flag), Some(&config_flag), |name| match name {
                "LOCKED_BUILD_STATE" => Some(root.path().join("env-state")),
                "LOCKED_BUILD_CONFIG" => Some(root.path().join("env-policy")),
                _ => None,
            })?;
        assert_eq!(resolved_state, state_flag);
        assert_eq!(resolved_config, config_flag);

        let (env_state, env_config) = resolve_paths(None, None, |name| match name {
            "HOME" => Some(home.clone()),
            "LOCKED_BUILD_STATE" => Some(root.path().join("env-state")),
            "LOCKED_BUILD_CONFIG" => Some(root.path().join("env-policy")),
            _ => None,
        })?;
        assert_eq!(env_state, root.path().join("env-state"));
        assert_eq!(env_config, root.path().join("env-policy"));
        Ok(())
    }

    /// Trace: FR-011-AC-7
    #[test]
    fn absent_run_class_defaults_to_periodic() {
        let parsed = Cli::try_parse_from([
            "locked-build",
            "run",
            "--owner",
            "owner",
            "--ticket",
            "T-1",
            "--",
            "true",
        ]);
        assert!(matches!(
            parsed,
            Ok(Cli {
                command: Action::Run {
                    class: None,
                    priority: None,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn cargo_shim_queues_the_real_binary_with_documented_defaults() -> Result<()> {
        let arguments = ["cargo", "check"]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        let real_cargo = std::path::Path::new("/bin/echo").canonicalize()?;
        let command = cargo_shim_command(CargoShimInvocation {
            arguments: &arguments,
            real_cargo: &real_cargo,
            config_path: std::path::Path::new("/policy/policy.json"),
            state_path: std::path::Path::new("/state"),
            class: DEFAULT_CARGO_CLASS,
            owner: DEFAULT_CARGO_OWNER,
            ticket: DEFAULT_CARGO_TICKET,
            in_lease: false,
        })?;
        let actual: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            actual,
            [
                "--state",
                "/state",
                "--config",
                "/policy/policy.json",
                "run",
                "--owner",
                "cargo-shim",
                "--ticket",
                "MANUAL",
                "--class",
                "point",
                "--",
                real_cargo.to_string_lossy().as_ref(),
                "check"
            ]
        );
        Ok(())
    }
}
