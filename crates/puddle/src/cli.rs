// SPDX-License-Identifier: AGPL-3.0-or-later
//! Command-line parsing for the host program, kept apart from `main` so it is unit-testable.

/// What the program should do, decided from its arguments.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Print the version line.
    Version,
    /// Print usage.
    Help,
    /// Relay stdio to a workspace's SSH endpoint (`ProxyCommand`), see [`crate::cmd::ssh_bridge`].
    SshBridge {
        /// The workspace's endpoint (named pipe or Unix socket path).
        endpoint: std::path::PathBuf,
    },
    /// Check this machine's prerequisites, see [`crate::cmd::doctor`].
    Doctor(crate::cmd::doctor::DoctorArgs),
    /// Copy the data folder's database, settings and workspace list, see [`crate::cmd::backup`].
    Backup(crate::cmd::backup::BackupArgs),
    /// Run the host process until asked to stop, see [`crate::cmd::serve`].
    Serve(crate::cmd::serve::ServeArgs),
    /// Take a user's password away and print a one-time invite link, see [`crate::cmd::user`].
    UserReset(crate::cmd::user::ResetArgs),
}

/// Arguments that don't form a valid command.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum UsageError {
    /// An argument Puddle doesn't know.
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    /// No command given.
    #[error("no command given")]
    Missing,
    /// An option's value is not one the option takes.
    #[error("{option} must be {expected}, found {value:?}")]
    InvalidValue {
        /// The flag.
        option: &'static str,
        /// What it takes.
        expected: &'static str,
        /// What was given.
        value: String,
    },
    /// A command lacks its argument; holds the usage of that command.
    #[error("missing argument: {0}")]
    MissingArgument(&'static str),
}

/// Usage text for `--help` and for usage errors.
pub const USAGE: &str = "usage: puddle [--version | --help | doctor [--json] [--no-boot] [--config <path>] [--data-dir <path>] | serve [--config <path>] [--print-config] [--port <n>] [--connection-file <path>] [--data-dir <path>] [--secret-store <store>] [--headless | --no-headless] [--log <filter>] [--remote | --no-remote] [--public-hosts <host,...>] [--workspace-base-domain <domain>] [--listen-interface <ip>] [--listen-port <n>] [--allowed-proxies <ip,...>] [--multi-user | --no-multi-user] | user reset [--config <path>] [--data-dir <path>] <name> | backup [--config <path>] [--data-dir <path>] <dest> | ssh-bridge <endpoint>]\nserve options and the config file: https://github.com/PuddleHQ/puddle/blob/main/docs/serve.md";

/// Parse the arguments after the program name.
///
/// # Errors
///
/// [`UsageError`] when the arguments are empty or contain anything other than one known flag.
pub fn parse<I, S>(args: I) -> Result<Command, UsageError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let command = match args.next() {
        None => return Err(UsageError::Missing),
        Some(arg) => match arg.as_ref() {
            "--version" | "-V" => Command::Version,
            "--help" | "-h" => Command::Help,
            "ssh-bridge" => return crate::cmd::ssh_bridge::parse(args),
            "doctor" => return crate::cmd::doctor::parse(args),
            "serve" => return crate::cmd::serve::parse(args),
            "backup" => return crate::cmd::backup::parse(args),
            "user" => return crate::cmd::user::parse(args),
            other => return Err(UsageError::UnknownArgument(other.to_owned())),
        },
    };
    match args.next() {
        None => Ok(command),
        Some(extra) => Err(UsageError::UnknownArgument(extra.as_ref().to_owned())),
    }
}

#[cfg(test)]
mod tests;
