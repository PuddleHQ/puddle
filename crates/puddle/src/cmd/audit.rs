// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle audit status|prune [--days <n>] [--config <path>] [--data-dir <path>]`: the operator's
//! view of the activity log. `status` says how big it is, how fast it grows and for how many days
//! it is kept; `prune` deletes the records older than that (or than `--days`) now and gives the
//! file's free space back. Deleted records cannot be brought back. Like `puddle user`, it works on
//! the database while the server runs.

use std::process::ExitCode;

use puddle_host::HostPaths;

use crate::cli::{Command, UsageError};
use crate::cmd::data_folder;
use crate::cmd::serve::options::Flags;

/// What `puddle audit` should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Show the size, growth and retention.
    Status,
    /// Delete the records older than the days (the saved retention when `None`).
    Prune(Option<u32>),
}

/// How `puddle audit` should run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditArgs {
    /// What to do.
    pub action: Action,
    /// `--config` and `--data-dir`, as given.
    pub flags: Flags,
}

const USAGE: &str = "audit status|prune [--days <n>]";

/// Parses the arguments after `audit`.
///
/// # Errors
///
/// [`UsageError`] for anything but `status` or `prune`, `--days` with a whole number (only for
/// `prune`), and at most one `--config` and one `--data-dir`.
pub fn parse<I, S>(args: I) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args;
    let word = args.next().ok_or(UsageError::MissingArgument(USAGE))?;
    let pruning = match word.as_ref() {
        "status" => false,
        "prune" => true,
        other => return Err(UsageError::UnknownArgument(other.to_owned())),
    };
    let mut days = None;
    let mut flags = Flags::new();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        if data_folder::take_option(arg, &mut args, &mut flags)? {
            continue;
        }
        if arg != "--days" || !pruning || days.is_some() {
            return Err(UsageError::UnknownArgument(arg.to_owned()));
        }
        let text = args
            .next()
            .ok_or(UsageError::MissingArgument("--days <n>"))?;
        days = Some(
            text.as_ref()
                .parse::<u32>()
                .map_err(|_| UsageError::InvalidValue {
                    option: "--days",
                    expected: "a whole number of days",
                    value: text.as_ref().to_owned(),
                })?,
        );
    }
    let action = if pruning {
        Action::Prune(days)
    } else {
        Action::Status
    };
    Ok(Command::Audit(AuditArgs { action, flags }))
}

/// Prints the result, or the failure on stderr.
#[expect(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a CLI's user-facing output goes to stdout/stderr, not to tracing"
)]
#[must_use]
pub fn run(args: &AuditArgs) -> ExitCode {
    match execute(args) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("puddle: {message}");
            ExitCode::FAILURE
        }
    }
}

fn paths(flags: &Flags) -> Result<HostPaths, String> {
    let data = match data_folder::configured_here(flags).map_err(|e| e.to_string())? {
        Some(dir) => dir,
        None => puddle_fs::data_dir().map_err(|e| e.to_string())?,
    };
    Ok(HostPaths::new(data))
}

fn mib(bytes: u64) -> String {
    let tenths = u128::from(bytes) * 10 / (1024 * 1024);
    format!("{}.{} MiB", tenths / 10, tenths % 10)
}

/// What `status` prints, with a warning when the log is over its warning size.
fn status_text(s: &puddle_host::AuditStats) -> String {
    let over = if s.bytes > s.warn_bytes {
        format!(
            "\nIt is over the {} where Puddle warns. Nothing is deleted for its size; \
             shorten the days or run `audit prune`.",
            mib(s.warn_bytes)
        )
    } else {
        String::new()
    };
    format!(
        "kept for {} days\n{} records, {} of text; the file holds {} ({} free)\n\
         written in the last day: {} records, {}; in the last week: {} records, {}{over}",
        s.retention_days,
        s.records,
        mib(s.bytes),
        mib(s.file_bytes),
        mib(s.free_bytes),
        s.records_last_day,
        mib(s.bytes_last_day),
        s.records_last_week,
        mib(s.bytes_last_week),
    )
}

fn execute(args: &AuditArgs) -> Result<String, String> {
    let paths = paths(&args.flags)?;
    match args.action {
        Action::Status => {
            let s = puddle_host::audit_status(&paths).map_err(|e| e.to_string())?;
            Ok(status_text(&s))
        }
        Action::Prune(days) => {
            let done = puddle_host::prune_audit(&paths, days).map_err(|e| e.to_string())?;
            Ok(format!(
                "deleted {} records older than {} days; the file gave back {}",
                done.deleted,
                done.days,
                mib(done.shrunk_bytes)
            ))
        }
    }
}

#[cfg(test)]
mod tests;
