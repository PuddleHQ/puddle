// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle backup [--data-dir <path>] <dest>`: a consistent copy of Puddle's database, settings
//! and workspace list in a new folder, made while the host runs (see [`puddle_host::backup`]).
//! The restore steps are in `docs/backup.md`.

use std::path::PathBuf;
use std::process::ExitCode;

use puddle_host::HostPaths;

use crate::cli::{Command, UsageError};

/// How `puddle backup` should run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupArgs {
    /// The new folder the backup goes in.
    pub dest: PathBuf,
    /// The data folder to read instead of the per-user one.
    pub data_dir: Option<PathBuf>,
}

/// Parses the arguments after `backup`: one destination, optionally `--data-dir <path>`.
///
/// # Errors
///
/// [`UsageError`] without a destination, with a second one, or with an unknown or repeated option.
pub fn parse<I, S>(args: I) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args;
    let mut dest = None;
    let mut data_dir = None;
    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--data-dir" if data_dir.is_none() => {
                let value = args
                    .next()
                    .ok_or(UsageError::MissingArgument("--data-dir <path>"))?;
                data_dir = Some(PathBuf::from(value.as_ref()));
            }
            other if !other.starts_with("--") && dest.is_none() => {
                dest = Some(PathBuf::from(other));
            }
            other => return Err(UsageError::UnknownArgument(other.to_owned())),
        }
    }
    let dest = dest.ok_or(UsageError::MissingArgument("backup <dest>"))?;
    Ok(Command::Backup(BackupArgs { dest, data_dir }))
}

/// Makes the backup. Exit code 0 when it is complete, 1 when it could not be made (nothing is
/// left at the destination then).
#[must_use]
#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a CLI's user-facing output goes to stdout/stderr, not to tracing"
)]
pub fn run(args: &BackupArgs) -> ExitCode {
    let data = match &args.data_dir {
        Some(dir) => dir.clone(),
        None => match puddle_fs::data_dir() {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("puddle: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    match puddle_host::backup(&HostPaths::new(data), &args.dest) {
        Ok(report) => {
            println!(
                "backup written to {}: database (schema version {}), {} settings file(s), workspace list {}",
                report.dest.display(),
                report.schema_version,
                report.settings_files,
                if report.workspace_list {
                    "included"
                } else {
                    "absent"
                },
            );
            println!(
                "not included: stored logins and secrets (they stay in the operating system's credential store), workspace volumes, the sandbox runtime"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("puddle: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_alone_and_with_a_data_dir() {
        assert_eq!(
            parse(["out"].into_iter()),
            Ok(Command::Backup(BackupArgs {
                dest: "out".into(),
                data_dir: None
            }))
        );
        assert_eq!(
            parse(["--data-dir", "d", "out"].into_iter()),
            Ok(Command::Backup(BackupArgs {
                dest: "out".into(),
                data_dir: Some("d".into())
            }))
        );
    }

    #[test]
    fn missing_or_extra_arguments_are_errors() {
        assert_eq!(
            parse(Vec::<&str>::new().into_iter()),
            Err(UsageError::MissingArgument("backup <dest>"))
        );
        assert_eq!(
            parse(["a", "b"].into_iter()),
            Err(UsageError::UnknownArgument("b".into()))
        );
        assert_eq!(
            parse(["--bogus"].into_iter()),
            Err(UsageError::UnknownArgument("--bogus".into()))
        );
        assert_eq!(
            parse(["--data-dir"].into_iter()),
            Err(UsageError::MissingArgument("--data-dir <path>"))
        );
    }
}
