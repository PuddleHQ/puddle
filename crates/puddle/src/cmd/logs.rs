// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle logs bundle [--output <path>] [--config <path>] [--data-dir <path>]`: one `.tar.gz` with
//! the versions, the platform, the settings (secrets cut out) and the recent log files, for "upload
//! a debug log". Every text in it goes through [`puddle_log::redact`]. The data folder is the one
//! `puddle serve` would use ([`crate::cmd::data_folder`]); the levels are in `docs/logging.md`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use puddle_log::{Bundle, write_bundle};

use crate::cli::{Command, UsageError};
use crate::cmd::data_folder;
use crate::cmd::serve::options::Flags;

/// How `puddle logs bundle` should run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogsArgs {
    /// Where the bundle goes; `puddle-logs-<time>.tar.gz` in the current folder when left out.
    pub output: Option<PathBuf>,
    /// `--data-dir` and `--config`, by option key; with the environment they pick the data folder
    /// as `puddle serve` does.
    pub flags: Flags,
}

/// Parses the arguments after `logs`: `bundle`, optionally `--output <path>`, `--data-dir <path>`
/// and `--config <path>`.
///
/// # Errors
///
/// [`UsageError`] without `bundle`, for an unknown or repeated option or a missing value.
pub fn parse<I, S>(args: I) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args;
    match args.next().as_ref().map(AsRef::as_ref) {
        Some("bundle") => {}
        Some(other) => return Err(UsageError::UnknownArgument(other.to_owned())),
        None => return Err(UsageError::MissingArgument("logs bundle")),
    }
    let mut parsed = LogsArgs::default();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        if data_folder::take_option(arg, &mut args, &mut parsed.flags)? {
            continue;
        }
        match arg {
            "--output" if parsed.output.is_none() => {
                let value = args
                    .next()
                    .ok_or(UsageError::MissingArgument("--output <path>"))?;
                parsed.output = Some(PathBuf::from(value.as_ref()));
            }
            other => return Err(UsageError::UnknownArgument(other.to_owned())),
        }
    }
    Ok(Command::Logs(parsed))
}

/// Writes the bundle. Exit code 0 with the path printed, 1 when it could not be made.
#[must_use]
#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a CLI's user-facing output goes to stdout/stderr, not to tracing"
)]
pub fn run(args: &LogsArgs) -> ExitCode {
    match make(args) {
        Ok(path) => {
            println!("log bundle written to {}", path.display());
            println!(
                "it holds the versions, the platform, the settings and the recent logs, with secrets cut out; read it before you share it"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("puddle: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Why the bundle could not be made; it displays as the line `run` prints.
#[derive(Debug, thiserror::Error)]
enum BundleError {
    /// The data folder could not be found.
    #[error(transparent)]
    DataFolder(#[from] crate::cmd::CommandError),
    /// The archive could not be built.
    #[error(transparent)]
    Archive(#[from] puddle_log::BundleError),
    /// The finished archive could not be written.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// The output path.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
}

fn make(args: &LogsArgs) -> Result<PathBuf, BundleError> {
    let paths = crate::cmd::data_folder_here(&args.flags)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("puddle-logs-{now}.tar.gz")));
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();
    let settings = read_settings(&paths.settings().join("global.json"));
    let level = std::env::var("PUDDLE_LOG").ok().unwrap_or_else(|| {
        settings
            .as_ref()
            .and_then(|s| s.get("ui")?.get("log_level")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| "info".to_owned())
    });
    let versions = [("puddle".to_owned(), puddle_types::VERSION.to_owned())];
    let platform = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    let bundle = Bundle {
        versions: &versions,
        platform: &platform,
        log_level: &level,
        settings,
        log_folder: &puddle_log::log_folder(paths.data()),
        home: home.as_deref(),
        generated: now,
    };
    // The file is made only once the whole bundle is in memory, so a failure leaves none behind.
    let mut bytes = Vec::new();
    write_bundle(&bundle, &mut bytes)?;
    std::fs::write(&output, bytes).map_err(|source| BundleError::Write {
        path: output.clone(),
        source,
    })?;
    Ok(output)
}

/// The global settings document, or none when there is no readable one.
fn read_settings(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests;
