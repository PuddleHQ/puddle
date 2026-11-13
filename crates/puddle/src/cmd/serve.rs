// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle serve`: runs the host process (store, proxies, sandbox runtime, workspaces, API)
//! until Ctrl-C, a termination signal or the end of the console session, then stops every
//! workspace cleanly.
//!
//! The options come from [`options`] (flags, environment, a `--config` file, defaults; see
//! `docs/serve.md`). The work is in [`puddle_host`]; this file is the process around it: the
//! Windows front/worker split, logging, the order "prepare before the first thread", and the exit
//! code.

use std::process::ExitCode;

use puddle_host::{Host, HostConfig, HostOptions, MsbFactory, SystemPlatform, prepare};
use puddle_lifecycle::{Role, supervise, wait_for_shutdown};
use puddle_log::init_stderr;

use crate::cli::{Command, UsageError};

pub mod config_file;
pub mod options;

use config_file::ConfigError;
use options::{Effective, Flags, Value, resolve, spec_for_flag, spec_for_negated_flag};

/// The environment variable that overrides the guest agent's location (development builds).
pub const AGENT_VAR: &str = "PUDDLE_AGENT_BIN";

/// The environment variable that overrides the bundled code-server folder (development builds).
pub const CODE_SERVER_VAR: &str = "PUDDLE_CODE_SERVER_DIR";

/// How `puddle serve` should run: the flags that were given and whether to only print the options.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServeArgs {
    /// The options given as flags, by option key (see [`options::OPTIONS`]).
    pub flags: Flags,
    /// `--print-config`: print the effective options and their sources, then exit.
    pub print_config: bool,
}

/// Parses the arguments after `serve`.
///
/// # Errors
///
/// [`UsageError`] for an unknown or repeated option or a missing or invalid value.
pub fn parse<I, S>(args: I) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut parsed = ServeArgs::default();
    let mut args = args;
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        if arg == PRINT_CONFIG_FLAG && !parsed.print_config {
            parsed.print_config = true;
            continue;
        }
        let unknown = || UsageError::UnknownArgument(arg.to_owned());
        if let Some(spec) = spec_for_flag(arg).filter(|spec| spec.kind.is_switch()) {
            // `--name`: a switch takes no value.
            if parsed.flags.insert(spec.key, Value::Bool(true)).is_some() {
                return Err(unknown());
            }
            continue;
        }
        if let Some(spec) = spec_for_negated_flag(arg) {
            if parsed.flags.insert(spec.key, Value::Bool(false)).is_some() {
                return Err(unknown());
            }
            continue;
        }
        let spec = spec_for_flag(arg)
            .filter(|spec| !parsed.flags.contains_key(spec.key))
            .ok_or_else(unknown)?;
        let text = args.next().ok_or(UsageError::MissingArgument(spec.usage))?;
        let value =
            spec.kind
                .parse_text(text.as_ref())
                .map_err(|expected| UsageError::InvalidValue {
                    option: spec.flag(),
                    expected,
                    value: text.as_ref().to_owned(),
                })?;
        parsed.flags.insert(spec.key, value);
    }
    Ok(Command::Serve(parsed))
}

/// The flag that prints the effective options and exits.
pub const PRINT_CONFIG_FLAG: &str = "--print-config";

/// The options for this process: its flags, its environment, the config file they name.
///
/// # Errors
///
/// [`ConfigError`] with the whole message when an environment variable or the config file is not
/// usable.
pub fn effective(args: &ServeArgs) -> Result<Effective, ConfigError> {
    let cwd = std::env::current_dir().map_err(|e| ConfigError::WorkingDirectory(e.to_string()))?;
    let options = resolve(&args.flags, &|name| std::env::var_os(name), &cwd)?;
    // Options that do not fit together refuse here, before the host prepares anything.
    options.remote()?;
    Ok(options)
}

/// The host configuration for this machine and these options.
///
/// # Errors
///
/// [`puddle_host::HostError`] when the data folder or runtime folder cannot be found.
pub fn config(
    options: &Effective,
    exe: &std::path::Path,
) -> Result<HostConfig, puddle_host::HostError> {
    let mut config = match options.path("data_dir") {
        Some(data) => HostConfig::installed(exe, data)?,
        None => HostConfig::installed_for_user(exe)?,
    };
    if let Some(agent) = std::env::var_os(AGENT_VAR) {
        config.guest.agent_binary = agent.into();
    }
    if let Some(dir) = std::env::var_os(CODE_SERVER_VAR) {
        config.guest.code_server = Some(dir.into());
    }
    if let Some(port) = options.port() {
        config.api.port = port;
    }
    if let Some(file) = options.path("connection_file") {
        config.api.connection_file = Some(file.to_path_buf());
    }
    let remote = options
        .remote()
        .map_err(|e| puddle_host::HostError::State {
            what: "remote serving",
            reason: e.to_string(),
        })?;
    config.api.multi_user = options.switch("multi_user") == Some(true);
    config.api.remote = remote;
    Ok(config)
}

/// Runs the host. Exit code 0 after a clean stop, 1 when it could not start or a workspace did
/// not stop.
#[must_use]
#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a daemon's start-up line and fatal errors go to the console"
)]
pub fn run(args: &ServeArgs) -> ExitCode {
    // Before anything starts, so a bad file or `--print-config` never reaches the host.
    let options = match effective(args) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("puddle: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.print_config {
        print!("{}", options.render());
        return ExitCode::SUCCESS;
    }
    // On Windows this is the front process, which only relays; the worker runs the host.
    match supervise() {
        Ok(Role::Front { exit_code }) => {
            return ExitCode::from(u8::try_from(exit_code).unwrap_or(1));
        }
        Ok(Role::Worker) => {}
        Err(e) => {
            eprintln!("puddle: {e}");
            return ExitCode::FAILURE;
        }
    }
    init_stderr(options.log_filter(), false);
    // Before the first credential is read; `PUDDLE_ALLOW_DUMPS=1` keeps native dumps for diagnosis.
    puddle_no_dump::harden_from_env().log();
    // Everything up to the async runtime happens while this is the only thread.
    let prepared = match std::env::current_exe()
        .map_err(|e| puddle_host::HostError::DataDir(e.to_string()))
        .and_then(|exe| config(&options, &exe))
        .and_then(|config| prepare(config, &SystemPlatform::new().with_dev_override_from_env()))
    {
        Ok(prepared) => prepared,
        Err(e) => {
            eprintln!("puddle: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("puddle: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let host = match Host::start(prepared, &MsbFactory, HostOptions::default()).await {
            Ok(host) => host,
            Err(e) => {
                eprintln!("puddle: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!("Puddle is up at {}", host.url());
        if let Some(listener) = host.remote_listener() {
            for line in listener_lines(listener) {
                println!("{line}");
            }
        }
        match wait_for_shutdown().await {
            Ok(cause) => tracing::info!(cause = cause.as_str(), "shutting down"),
            Err(e) => tracing::error!(error = %e, "cannot wait for a shutdown request; stopping"),
        }
        exit_after(&host.shutdown().await.sandboxes)
    })
}

/// What `puddle serve` prints about the network listener: where it is, and what the proxy's
/// operator trusts or pins.
fn listener_lines(listener: &puddle_host::RemoteListener) -> [String; 3] {
    [
        format!("Network listener (TLS) on {}", listener.addr),
        format!("  certificate SHA-256: {}", listener.fingerprint),
        format!(
            "  certificate file: {}",
            listener.certificate_file.display()
        ),
    ]
}

/// Prints what is wrong with the sandboxes at exit, and picks the exit code.
#[expect(
    clippy::print_stderr,
    reason = "the daemon's last words go to the console"
)]
fn exit_after(report: &puddle_lifecycle::ShutdownReport) -> ExitCode {
    for line in shutdown_lines(report) {
        eprintln!("{line}");
    }
    if report.all_stopped() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// What Puddle tells the user at exit about sandboxes that did not stop or trim cleanly:
/// nothing when all did.
fn shutdown_lines(report: &puddle_lifecycle::ShutdownReport) -> Vec<String> {
    let mut lines: Vec<String> = report
        .untrimmed()
        .into_iter()
        .map(|(sandbox, why)| {
            format!(
                "puddle: the disk of {sandbox} was not trimmed: {why}; Reclaim space trims it later"
            )
        })
        .collect();
    let unstopped = report.unstopped();
    if !unstopped.is_empty() {
        lines.push("puddle: not every sandbox stopped cleanly:".to_owned());
        lines.extend(
            unstopped
                .into_iter()
                .map(|(sandbox, why)| format!("puddle:   {sandbox}: {why}")),
        );
        lines.push(
            "puddle: the machines end with Puddle; check the workspace volumes if a tool was writing"
                .to_owned(),
        );
    }
    lines
}

#[cfg(test)]
mod tests;
