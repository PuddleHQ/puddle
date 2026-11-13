// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle user reset [--config <path>] [--data-dir <path>] <name>`: gives a user back their way
//! in. It takes the user's password away, signs every one of their browsers out, and prints a
//! one-time invite link that sets a new password. It never prints or sets a password.
//!
//! It reads the same options as `puddle serve` (flags, environment, the config file) to know the
//! data folder and the public address the link points at, and works on the database while the
//! server runs.

use std::io::Write;
use std::process::ExitCode;

use puddle_host::HostPaths;

use crate::cli::{Command, UsageError};
use crate::cmd::data_folder;
use crate::cmd::serve::options::{Effective, Flags};
use crate::cmd::serve::{ServeArgs, effective};

/// How `puddle user reset` should run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetArgs {
    /// The user to reset.
    pub name: String,
    /// `--config` and `--data-dir`, as given (the options `puddle serve` reads, by key).
    pub flags: Flags,
}

/// Parses the arguments after `user`.
///
/// # Errors
///
/// [`UsageError`] for anything but `reset`, a name, and at most one `--config` and one
/// `--data-dir`.
pub fn parse<I, S>(args: I) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args;
    match args.next() {
        Some(word) if word.as_ref() == "reset" => {}
        Some(other) => return Err(UsageError::UnknownArgument(other.as_ref().to_owned())),
        None => return Err(UsageError::MissingArgument("user reset <name>")),
    }
    let mut name = None;
    let mut flags = Flags::new();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        if data_folder::take_option(arg, &mut args, &mut flags)? {
            continue;
        }
        if arg.starts_with('-') || name.is_some() {
            return Err(UsageError::UnknownArgument(arg.to_owned()));
        }
        name = Some(arg.to_owned());
    }
    let name = name.ok_or(UsageError::MissingArgument("user reset <name>"))?;
    Ok(Command::UserReset(ResetArgs { name, flags }))
}

/// What the operator is told: the link on its own line (so it can be piped), the rest on stderr.
#[expect(
    clippy::print_stderr,
    reason = "a CLI's user-facing output goes to stdout/stderr, not to tracing"
)]
#[must_use]
pub fn run(args: &ResetArgs) -> ExitCode {
    match execute(args) {
        Ok(link) => {
            eprintln!(
                "{}'s password is removed and every browser they were signed in on is signed out.\n\
                 Open this link in a browser within 24 hours to set a new password; it works once:",
                args.name
            );
            // The link is the command's result, not a log line, so it is written to stdout
            // directly: a print macro is a logging sink and this carries a one-time secret.
            print_link(&mut std::io::stdout().lock(), &link)
        }
        Err(message) => {
            eprintln!("puddle: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Writes the link and says so when it cannot: a reset whose link got lost is a failure.
#[expect(clippy::print_stderr, reason = "a CLI's user-facing output")]
fn print_link(out: &mut impl Write, link: &str) -> ExitCode {
    match writeln!(out, "{link}") {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("puddle: could not write the link: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resets the user and returns the invite link.
fn execute(args: &ResetArgs) -> Result<String, String> {
    let options = effective(&ServeArgs {
        flags: args.flags.clone(),
        print_config: false,
    })
    .map_err(|e| e.to_string())?;
    let origin = invite_origin(&options)?;
    let data = match options.path("data_dir") {
        Some(dir) => dir.to_path_buf(),
        None => puddle_fs::data_dir().map_err(|e| e.to_string())?,
    };
    let invite =
        puddle_host::reset_user(&HostPaths::new(data), &args.name).map_err(|e| e.to_string())?;
    Ok(format!("{origin}/invite#{}", invite.secret.as_str()))
}

/// The address an invite link points at: the first public host. Checked before anything is
/// changed, so a reset never leaves a user without a password and without a link.
fn invite_origin(options: &Effective) -> Result<String, String> {
    let remote = options.remote().map_err(|e| e.to_string())?;
    remote
        .and_then(|r| {
            r.serving
                .public_hosts()
                .first()
                .map(puddle_api::PublicHost::origin)
        })
        .ok_or_else(|| {
            "remote serving is not on in these options, so there is no public address for the \
             link; give the same --config (or PUDDLE_REMOTE and PUDDLE_PUBLIC_HOSTS) as the server"
                .to_owned()
        })
}

#[cfg(test)]
mod tests;
