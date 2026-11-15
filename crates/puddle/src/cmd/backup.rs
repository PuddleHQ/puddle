// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle backup [--config <path>] [--data-dir <path>] <dest>`: a consistent copy of Puddle's
//! database (without the sign-in secrets), settings and workspace list in a new folder, made while
//! the host runs (see [`puddle_host::backup`]). The data folder is the one `puddle serve` would use
//! ([`crate::cmd::data_folder`]). The restore steps are in `docs/backup.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use puddle_host::HostPaths;

use crate::cli::{Command, UsageError};
use crate::cmd::data_folder;
use crate::cmd::serve::options::Flags;

/// How `puddle backup` should run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupArgs {
    /// The new folder the backup goes in.
    pub dest: PathBuf,
    /// `--data-dir` and `--config`, by option key; with the environment they pick the data folder
    /// as `puddle serve` does.
    pub flags: Flags,
}

/// Parses the arguments after `backup`: one destination, optionally `--data-dir <path>` and `--config <path>`.
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
    let mut flags = Flags::new();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        if data_folder::take_option(arg, &mut args, &mut flags)? {
            continue;
        }
        if !arg.starts_with("--") && dest.is_none() {
            dest = Some(PathBuf::from(arg));
        } else {
            return Err(UsageError::UnknownArgument(arg.to_owned()));
        }
    }
    let dest = dest.ok_or(UsageError::MissingArgument("backup <dest>"))?;
    Ok(Command::Backup(BackupArgs { dest, flags }))
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
    let located = std::env::current_dir()
        .map_err(|e| e.to_string())
        .and_then(|cwd| {
            execute(
                args,
                &|name| std::env::var_os(name),
                &cwd,
                puddle_fs::data_dir(),
            )
        });
    match located {
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
                "not included: stored logins and secrets (they stay in the operating system's credential store), account passwords and open invites (people get a new invite after a restore), workspace volumes, the sandbox runtime"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("puddle: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Makes the backup of the folder the options name (`args.flags`, `env`, `cwd`), or of
/// `default_data_dir` (the per-user folder, or why there is none) when none names one.
fn execute(
    args: &BackupArgs,
    env: &dyn Fn(&str) -> Option<OsString>,
    cwd: &Path,
    default_data_dir: Result<PathBuf, puddle_fs::DataDirError>,
) -> Result<puddle_host::BackupReport, String> {
    let data = match data_folder::configured(&args.flags, env, cwd).map_err(|e| e.to_string())? {
        Some(dir) => dir,
        None => default_data_dir.map_err(|e| e.to_string())?,
    };
    puddle_host::backup(&HostPaths::new(data), &args.dest).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
