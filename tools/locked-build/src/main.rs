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
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use clap::{Parser, Subcommand};

use crate::error::Result;
use crate::model::RequestId;

const DEFAULT_CARGO_CLASS: &str = "point";
const DEFAULT_CARGO_TICKET: &str = "MANUAL";

#[derive(Parser)]
#[command(version, about = "Fair persistent build admission for shared hosts")]
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
        owner: Option<String>,
        #[arg(long)]
        ticket: Option<String>,
        #[arg(long, default_value_t = 1)]
        units: u32,
        #[arg(long)]
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
    /// Show or initialize the effective host policy.
    Policy {
        #[command(subcommand)]
        command: PolicyAction,
    },
    #[command(hide = true)]
    Worker { request: String },
    #[command(hide = true)]
    Watchdog { request: String, generation: u64 },
    #[command(hide = true)]
    Parking,
}

#[derive(Subcommand)]
enum PolicyAction {
    /// Print the effective host policy as JSON.
    Show,
    /// Write a measured default policy; use --force to replace an existing file.
    Init {
        #[arg(long)]
        force: bool,
    },
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
    if let Action::Policy { command } = &cli.command {
        match command {
            PolicyAction::Show => {
                ensure_policy(&config, false)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&crate::config::Config::read(&config)?)?
                );
                return Ok(0);
            }
            PolicyAction::Init { force } => {
                ensure_policy(&config, *force)?;
                return Ok(0);
            }
        }
    }
    ensure_policy(&config, false)?;
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
            class,
            command,
        } => {
            let owner = owner.unwrap_or_else(default_owner);
            let ticket = ticket.unwrap_or_else(default_ticket);
            let class_name = class.as_deref().unwrap_or("periodic");
            let priority = runtime::priority_for_class(&store, class_name)?;
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
        Action::Policy { .. } => {
            unreachable!("policy commands handled before opening scheduler state")
        }
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

fn ensure_policy(path: &std::path::Path, force: bool) -> Result<()> {
    let current_exe = std::env::current_exe()?;
    ensure_policy_with_discovery(
        path,
        force,
        std::env::var_os("PATH").as_deref(),
        &current_exe,
    )
}

fn ensure_policy_with_discovery(
    path: &std::path::Path,
    force: bool,
    search_path: Option<&OsStr>,
    current_exe: &std::path::Path,
) -> Result<()> {
    if path.exists() && !force {
        crate::config::Config::read(path)?;
        return Ok(());
    }
    let mut policy = crate::config::Config::measured_default()?;
    policy.real_cargo =
        search_path.and_then(|search_path| discover_real_cargo(search_path, current_exe));
    let parent = path.parent().ok_or_else(|| {
        crate::error::Error::new(
            crate::error::ErrorCode::Configuration,
            "policy path has no parent",
        )
    })?;
    let parent = if parent.as_os_str().is_empty() {
        std::path::Path::new(".")
    } else {
        parent
    };
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let bytes = serde_json::to_vec_pretty(&policy)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = options.mode(0o600).open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists && !force {
            crate::error::Error::new(
                crate::error::ErrorCode::Configuration,
                "policy was created concurrently; existing file was preserved",
            )
        } else {
            error.into()
        }
    })?;
    use std::io::Write;
    file.write_all(&bytes)?;
    file.sync_all()?;
    eprintln!("Wrote default policy to {}", path.display());
    Ok(())
}

fn discover_real_cargo(search_path: &OsStr, current_exe: &std::path::Path) -> Option<PathBuf> {
    let current_exe = current_exe.canonicalize().ok()?;
    std::env::split_paths(search_path).find_map(|directory| {
        let candidate = directory.join("cargo").canonicalize().ok()?;
        if candidate == current_exe {
            return None;
        }
        let metadata = std::fs::metadata(&candidate).ok()?;
        if !metadata.is_file() {
            return None;
        }
        use std::os::unix::fs::PermissionsExt;
        (metadata.permissions().mode() & 0o111 != 0).then_some(candidate)
    })
}

fn read_shim_config(path: &std::path::Path) -> Result<config::Config> {
    let current_exe = std::env::current_exe()?;
    read_shim_config_with_discovery(path, std::env::var_os("PATH").as_deref(), &current_exe)
}

fn read_shim_config_with_discovery(
    path: &std::path::Path,
    search_path: Option<&OsStr>,
    current_exe: &std::path::Path,
) -> Result<config::Config> {
    ensure_policy_with_discovery(path, false, search_path, current_exe)?;
    config::Config::read(path)
}

fn default_owner() -> String {
    if let Some(owner) = std::env::var_os("LOCKED_BUILD_OWNER") {
        return owner.to_string_lossy().into_owned();
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_owned());
    let host = sysinfo::System::host_name().unwrap_or_else(|| "localhost".to_owned());
    let cwd = std::env::current_dir().ok();
    derive_owner(&user, &host, cwd.as_deref())
}

fn derive_owner(user: &str, host: &str, cwd: Option<&std::path::Path>) -> String {
    let directory = cwd
        .and_then(std::path::Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_owned());
    format!("{user}@{host}:{directory}")
}

fn cargo_shim_owner(
    read_env: impl FnOnce(&str) -> Option<String>,
    fallback: impl FnOnce() -> String,
) -> String {
    read_env("LOCKED_BUILD_OWNER").unwrap_or_else(fallback)
}

fn default_ticket() -> String {
    if let Some(ticket) = std::env::var_os("LOCKED_BUILD_TICKET") {
        return ticket.to_string_lossy().into_owned();
    }
    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned());
    branch
        .as_deref()
        .and_then(ticket_from_branch)
        .unwrap_or_else(|| "-".to_owned())
}

fn ticket_from_branch(branch: &str) -> Option<String> {
    let bytes = branch.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_alphabetic() {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
            end += 1;
        }
        if end < bytes.len() && bytes[end] == b'-' {
            let digits = end + 1;
            let mut stop = digits;
            while stop < bytes.len() && bytes[stop].is_ascii_digit() {
                stop += 1;
            }
            if stop > digits {
                return Some(branch[start..stop].to_ascii_uppercase());
            }
        }
        start = end.max(start + 1);
    }
    None
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
    let config = read_shim_config(&config_path)?;
    let real_cargo = config.real_cargo.as_deref().ok_or_else(|| {
        error::Error::new(
            error::ErrorCode::Configuration,
            "real_cargo is not configured",
        )
    })?;
    let class = cargo_shim_class(|name| std::env::var(name).ok());
    let owner = cargo_shim_owner(|name| std::env::var(name).ok(), default_owner);
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

fn cargo_shim_class(read_env: impl FnOnce(&str) -> Option<String>) -> String {
    read_env("LOCKED_BUILD_CLASS").unwrap_or_else(|| DEFAULT_CARGO_CLASS.to_owned())
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
    fn run_accepts_named_class_without_owner_or_ticket_flags() {
        let parsed = Cli::try_parse_from([
            "locked-build",
            "run",
            "--class",
            "must-fix",
            "--",
            "cargo",
            "test",
        ]);
        assert!(
            matches!(parsed, Ok(Cli { command: Action::Run { owner: None, ticket: None, class: Some(class), .. }, .. }) if class == "must-fix")
        );
    }

    #[test]
    fn class_defaults_are_distinct_for_direct_run_and_cargo_shim() {
        assert_eq!(cargo_shim_class(|_| None), "point");
        assert_eq!(cargo_shim_class(|_| Some("block".to_owned())), "block");
        let direct_run = Cli::try_parse_from(["locked-build", "run", "--", "true"]);
        assert!(matches!(
            direct_run,
            Ok(Cli {
                command: Action::Run { class: None, .. },
                ..
            })
        ));
    }

    #[test]
    fn ticket_is_derived_from_a_ticket_branch_prefix() {
        assert_eq!(
            ticket_from_branch("tooling/qsl-3541-w1-2").as_deref(),
            Some("QSL-3541")
        );
        assert_eq!(ticket_from_branch("feature/no-ticket"), None);
    }

    #[test]
    fn cargo_shim_owner_uses_the_same_per_worktree_default_as_run() {
        let expected = derive_owner(
            "qa",
            "builder",
            Some(std::path::Path::new("/repo/worktrees/topic-a")),
        );
        assert_eq!(expected, "qa@builder:topic-a");
        assert_eq!(
            cargo_shim_owner(|_| None, || expected.clone()),
            "qa@builder:topic-a"
        );
        assert_eq!(
            cargo_shim_owner(
                |_| Some("override-owner".to_owned()),
                || "unused".to_owned()
            ),
            "override-owner"
        );
    }

    #[test]
    fn fresh_shim_policy_discovers_cargo_after_skipping_the_shim_binary() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir()?;
        let shim_dir = root.path().join("shim");
        let cargo_dir = root.path().join("real");
        std::fs::create_dir(&shim_dir)?;
        std::fs::create_dir(&cargo_dir)?;
        let current_exe = std::env::current_exe()?;
        symlink(&current_exe, shim_dir.join("cargo"))?;
        let real_cargo = cargo_dir.join("cargo");
        std::fs::write(&real_cargo, "#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(&real_cargo, std::fs::Permissions::from_mode(0o755))?;
        let search_path = std::env::join_paths([&shim_dir, &cargo_dir]).map_err(|error| {
            error::Error::new(error::ErrorCode::Configuration, error.to_string())
        })?;
        let config_path = root.path().join("config/locked-build/policy.json");

        let config =
            read_shim_config_with_discovery(&config_path, Some(&search_path), &current_exe)?;

        assert_eq!(config.real_cargo, Some(real_cargo.canonicalize()?));
        assert!(config_path.exists());
        Ok(())
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
        let owner = derive_owner("qa", "builder", Some(std::path::Path::new("/repo/topic-a")));
        let command = cargo_shim_command(CargoShimInvocation {
            arguments: &arguments,
            real_cargo: &real_cargo,
            config_path: std::path::Path::new("/policy.json"),
            state_path: std::path::Path::new("/state"),
            class: DEFAULT_CARGO_CLASS,
            owner: &owner,
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
        let parsed = Cli::try_parse_from(["locked-build", "run", "--", "true"]);
        assert!(matches!(
            parsed,
            Ok(Cli {
                command: Action::Run {
                    class: None,
                    owner: None,
                    ticket: None,
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
        let owner = derive_owner("qa", "builder", Some(std::path::Path::new("/repo/topic-a")));
        let command = cargo_shim_command(CargoShimInvocation {
            arguments: &arguments,
            real_cargo: &real_cargo,
            config_path: std::path::Path::new("/policy/policy.json"),
            state_path: std::path::Path::new("/state"),
            class: DEFAULT_CARGO_CLASS,
            owner: &owner,
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
                "qa@builder:topic-a",
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
