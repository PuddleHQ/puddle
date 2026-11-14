// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle user add|list|remove|reset [--config <path>] [--data-dir <path>] [<name>]`: the
//! operator's way to manage accounts on the server. `add` makes a user and prints a one-time
//! invite link; `reset` takes a user's password away, signs every browser out and prints a link;
//! `remove` starts deleting a user; `list` shows who is there. None of them prints or sets a
//! password.
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

/// What `puddle user` should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Make a user and print their invite link.
    Add(String),
    /// Show every user.
    List,
    /// Delete a user and everything of theirs.
    Remove(String),
    /// Take a user's password away and print an invite link.
    Reset(String),
}

/// How `puddle user` should run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserArgs {
    /// What to do.
    pub action: Action,
    /// `--config` and `--data-dir`, as given (the options `puddle serve` reads, by key).
    pub flags: Flags,
}

const USAGE_ADD: &str = "user add <name>";
const USAGE_LIST: &str = "user list";
const USAGE_REMOVE: &str = "user remove <name>";
const USAGE_RESET: &str = "user reset <name>";
const USAGE_ANY: &str = "user add|list|remove|reset";

/// Parses the arguments after `user`.
///
/// # Errors
///
/// [`UsageError`] for anything but `add`, `remove` or `reset` with a name, or `list`, and at most
/// one `--config` and one `--data-dir`.
pub fn parse<I, S>(args: I) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args;
    let (make, usage): (fn(String) -> Action, &'static str) = match args.next() {
        Some(word) => match word.as_ref() {
            "add" => (Action::Add, USAGE_ADD),
            "remove" => (Action::Remove, USAGE_REMOVE),
            "reset" => (Action::Reset, USAGE_RESET),
            "list" => return rest(args, None, USAGE_LIST),
            other => return Err(UsageError::UnknownArgument(other.to_owned())),
        },
        None => return Err(UsageError::MissingArgument(USAGE_ANY)),
    };
    rest(args, Some(make), usage)
}

/// The name (for the actions that take one) and the options.
fn rest<I, S>(
    mut args: I,
    make: Option<fn(String) -> Action>,
    usage: &'static str,
) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut name = None;
    let mut flags = Flags::new();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        if data_folder::take_option(arg, &mut args, &mut flags)? {
            continue;
        }
        if arg.starts_with('-') || make.is_none() || name.is_some() {
            return Err(UsageError::UnknownArgument(arg.to_owned()));
        }
        name = Some(arg.to_owned());
    }
    let action = match make {
        Some(make) => make(name.ok_or(UsageError::MissingArgument(usage))?),
        None => Action::List,
    };
    Ok(Command::User(UserArgs { action, flags }))
}

/// What a finished action has to say: `notice` for the person at the terminal (stderr) and
/// `result` for a pipe (stdout).
#[derive(Debug, PartialEq, Eq)]
struct Output {
    notice: String,
    result: String,
}

/// What the operator is told: the result on its own (so it can be piped), the rest on stderr.
#[expect(
    clippy::print_stderr,
    reason = "a CLI's user-facing output goes to stdout/stderr, not to tracing"
)]
#[must_use]
pub fn run(args: &UserArgs) -> ExitCode {
    match execute(args) {
        Ok(out) => {
            if !out.notice.is_empty() {
                eprintln!("{}", out.notice);
            }
            // The result is the command's output, not a log line, so it is written to stdout
            // directly: a print macro is a logging sink and an invite link is a one-time secret.
            write_result(&mut std::io::stdout().lock(), &out.result)
        }
        Err(message) => {
            eprintln!("puddle: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Writes the result and says so when it cannot: an add whose link got lost is a failure.
#[expect(clippy::print_stderr, reason = "a CLI's user-facing output")]
fn write_result(out: &mut impl Write, result: &str) -> ExitCode {
    if result.is_empty() {
        return ExitCode::SUCCESS;
    }
    match writeln!(out, "{result}") {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("puddle: could not write the result: {e}");
            ExitCode::FAILURE
        }
    }
}

fn execute(args: &UserArgs) -> Result<Output, String> {
    let options = effective(&ServeArgs {
        flags: args.flags.clone(),
        print_config: false,
    })
    .map_err(|e| e.to_string())?;
    let data = match options.path("data_dir") {
        Some(dir) => dir.to_path_buf(),
        None => puddle_fs::data_dir().map_err(|e| e.to_string())?,
    };
    let paths = HostPaths::new(data);
    match &args.action {
        Action::Reset(name) => {
            let origin = invite_origin(&options)?;
            let invite = puddle_host::reset_user(&paths, name).map_err(|e| e.to_string())?;
            Ok(Output {
                notice: format!(
                    "{name}'s password is removed and every browser they were signed in on is \
                     signed out.\nOpen this link in a browser within 24 hours to set a new \
                     password; it works once:"
                ),
                result: format!("{origin}/invite#{}", invite.secret.as_str()),
            })
        }
        Action::Add(name) => {
            let origin = invite_origin(&options)?;
            let invite = puddle_host::add_user(&paths, name).map_err(|e| e.to_string())?;
            Ok(Output {
                notice: format!(
                    "{name} is added. Give them this link; they open it within 24 hours and \
                     choose their own password. It works once:"
                ),
                result: format!("{origin}/invite#{}", invite.secret.as_str()),
            })
        }
        Action::Remove(name) => {
            puddle_host::remove_user(&paths, name).map_err(|e| e.to_string())?;
            Ok(Output {
                notice: format!(
                    "{name} can no longer sign in. Their workspaces and everything else of \
                     theirs are stopped and deleted by the running server within moments (or \
                     when it next starts). This cannot be undone."
                ),
                result: String::new(),
            })
        }
        Action::List => {
            let rows = puddle_host::list_users(&paths).map_err(|e| e.to_string())?;
            Ok(Output {
                notice: String::new(),
                result: table(&rows, now_ms()),
            })
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// One line per user: name, role, state, browsers signed in, last sign-in.
fn table(rows: &[puddle_host::UserRow], now: u64) -> String {
    let width = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    let mut lines = vec![format!(
        "{:<width$}  {:<5}  {:<8}  {:>7}  last sign-in",
        "name", "role", "state", "devices"
    )];
    for r in rows {
        let state = match r.state {
            puddle_host::UserState::Invited => "invited",
            puddle_host::UserState::Active => "active",
            puddle_host::UserState::Deleting => "deleting",
        };
        lines.push(format!(
            "{:<width$}  {:<5}  {:<8}  {:>7}  {}",
            r.name,
            if r.owner { "owner" } else { "user" },
            state,
            r.devices,
            ago(now, r.last_sign_in)
        ));
    }
    lines.join("\n")
}

/// How long ago `then` was, in the largest whole unit.
fn ago(now: u64, then: Option<u64>) -> String {
    let Some(then) = then else {
        return "never".to_owned();
    };
    let secs = now.saturating_sub(then) / 1000;
    match secs {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
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
