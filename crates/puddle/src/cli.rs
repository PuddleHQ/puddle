// SPDX-License-Identifier: GPL-3.0-or-later
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
    /// A command lacks its argument; holds the usage of that command.
    #[error("missing argument: {0}")]
    MissingArgument(&'static str),
}

/// Usage text for `--help` and for usage errors.
pub const USAGE: &str = "usage: puddle [--version | --help | doctor [--json] [--no-boot] | serve [--port <n>] [--connection-file <path>] | backup [--data-dir <path>] <dest> | ssh-bridge <endpoint>]";

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
