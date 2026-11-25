// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle ssh-bridge <endpoint>`: the `ProxyCommand` that connects `ssh` (or VS Code) to a
//! workspace's SSH endpoint. Puddle's ssh config file names the endpoint; the relay rules are
//! in [`puddle_ssh::bridge`].

use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cli::{Command, UsageError};

/// Parses the arguments after `ssh-bridge`: exactly one endpoint path.
///
/// # Errors
///
/// [`UsageError::MissingArgument`] without one, [`UsageError::UnknownArgument`] for more.
pub fn parse<I, S>(mut args: I) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let endpoint = args
        .next()
        .ok_or(UsageError::MissingArgument("ssh-bridge <endpoint>"))?;
    if let Some(extra) = args.next() {
        return Err(UsageError::UnknownArgument(extra.as_ref().to_owned()));
    }
    Ok(Command::SshBridge {
        endpoint: PathBuf::from(endpoint.as_ref()),
    })
}

/// When set, `run` prints how the session ended (the VM tests read it from the client's stderr).
const REPORT_VAR: &str = "PUDDLE_SSH_BRIDGE_REPORT";

/// Relays this process's stdin and stdout to `endpoint` until the session ends.
#[must_use]
#[expect(
    clippy::print_stderr,
    reason = "a ProxyCommand's only channel to the user is stderr, which ssh shows"
)]
pub fn run(endpoint: &Path) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("puddle: could not start the ssh bridge: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = rt.block_on(puddle_ssh::bridge::run(
        endpoint,
        tokio::io::stdin(),
        tokio::io::stdout(),
    ));
    // tokio reads stdin on a blocking thread that may sit in a read nobody will finish (ssh can
    // keep the pipe open): don't wait for it.
    rt.shutdown_background();
    finish(
        &result,
        std::env::var_os(REPORT_VAR).is_some(),
        &mut std::io::stderr(),
    )
}

/// Turns the relay's result into the exit code, telling `err` how it ended: a failure always, a
/// finished session only when `report` is set (a line on every session would show in every
/// user's ssh).
fn finish(
    result: &Result<impl std::fmt::Debug, puddle_ssh::bridge::BridgeError>,
    report: bool,
    err: &mut impl std::io::Write,
) -> ExitCode {
    match result {
        Ok(done) => {
            if report {
                // Nobody is left to tell if stderr is gone.
                let _ignored = writeln!(err, "puddle: ssh bridge ended: {done:?}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ignored = writeln!(err, "puddle: {}", with_causes(e));
            ExitCode::FAILURE
        }
    }
}

/// `e` followed by each of its sources, `: `-separated.
fn with_causes(e: &dyn Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        // Writing to a String can't fail.
        let _ignored = write!(text, ": {s}");
        source = s.source();
    }
    text
}

#[cfg(test)]
mod tests;
