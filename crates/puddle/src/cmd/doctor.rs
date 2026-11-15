// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle doctor [--json] [--no-boot] [--config <path>] [--data-dir <path>]`: checks this
//! machine's prerequisites and what can get in the way, with the exact fix for each problem
//! ([`puddle_doctor`]). Exit code 0 when nothing failed, 1 when something must be fixed first, so
//! scripts can gate on it. The data folder it checks is the one `puddle serve` would use
//! ([`crate::cmd::data_folder`]).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use puddle_doctor::boot::boot_logs_dir;
use puddle_doctor::{Options, Probe, Report, SystemProbe, diagnose};
use puddle_host::{FileSettings, HostPaths, disk_limits, ssh_agent_pipe};

use crate::cli::{Command, UsageError};
use crate::cmd::data_folder;
use crate::cmd::serve::options::Flags;

/// How `puddle doctor` should report.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DoctorArgs {
    /// Print JSON (with `schema_version`) instead of text.
    pub json: bool,
    /// Skip the test boot.
    pub no_boot: bool,
    /// `--data-dir` and `--config`, by option key; with the environment they pick the data folder
    /// as `puddle serve` does.
    pub flags: Flags,
}

/// Parses the arguments after `doctor`: `--json`, `--no-boot`, `--data-dir <path>` and `--config <path>`, each at most once.
///
/// # Errors
///
/// [`UsageError::UnknownArgument`] for anything else or a repeated flag, and
/// [`UsageError::MissingArgument`] for a path option without its value.
pub fn parse<I, S>(args: I) -> Result<Command, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut parsed = DoctorArgs::default();
    let mut args = args;
    while let Some(arg) = args.next() {
        if data_folder::take_option(arg.as_ref(), &mut args, &mut parsed.flags)? {
            continue;
        }
        let flag = match arg.as_ref() {
            "--json" => &mut parsed.json,
            "--no-boot" => &mut parsed.no_boot,
            other => return Err(UsageError::UnknownArgument(other.to_owned())),
        };
        if *flag {
            return Err(UsageError::UnknownArgument(arg.as_ref().to_owned()));
        }
        *flag = true;
    }
    Ok(Command::Doctor(parsed))
}

/// Runs the checks on this machine against the runtime next to this program and prints the
/// report.
#[must_use]
#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the report is the command's output"
)]
pub fn run(args: &DoctorArgs) -> ExitCode {
    let configured = match data_folder::configured_here(&args.flags) {
        Ok(configured) => configured,
        Err(e) => {
            eprintln!("puddle: {e}");
            return ExitCode::FAILURE;
        }
    };
    let data = data_folder(configured.map_or_else(puddle_fs::data_dir, Ok));
    let probe = match std::env::current_exe()
        .map_err(|e| e.to_string())
        .and_then(|exe| SystemProbe::installed(&exe).map_err(|e| e.to_string()))
        .map(|probe| keeping_logs(probe, data.as_deref()))
        .map(|probe| probe.with_ssh_agent_pipe(ssh_pipe(data.as_deref())))
    {
        Ok(probe) => probe,
        Err(e) => {
            eprintln!("puddle: can't find Puddle's own folder: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (output, code) = report(&probe, args, free_space_lines(data.as_deref()));
    print!("{output}");
    code
}

/// Puddle's data folder, or `None` after saying on stderr that a failed boot's logs then go
/// unkept.
#[expect(clippy::print_stderr, reason = "a diagnostic note beside the report")]
fn data_folder(found: Result<PathBuf, puddle_fs::DataDirError>) -> Option<PathBuf> {
    match found {
        Ok(dir) => Some(dir),
        Err(e) => {
            eprintln!("puddle: a failed test boot's logs will not be kept: {e}");
            None
        }
    }
}

/// `probe`, keeping a failed test boot's logs in the data folder (`data`) like the system check
/// in the app does, and measuring the drive that holds it and checking the folders above it.
/// Without a data folder nothing is kept and the disk and path checks say so.
fn keeping_logs(probe: SystemProbe, data: Option<&Path>) -> SystemProbe {
    match data {
        Some(data) => probe
            .with_keep_boot_logs(boot_logs_dir(data))
            .with_data_dir(data),
        None => probe,
    }
}

/// The free-space lines (warn, fail) in bytes, from the settings in the data folder the app
/// writes, or Puddle's own when there is no folder, no settings file or the file is not usable.
fn free_space_lines(data: Option<&Path>) -> (u64, u64) {
    let limits = match data {
        Some(data) => disk_limits(&FileSettings::new(HostPaths::new(data).settings())),
        None => puddle_settings::DiskSpaceLimits::default(),
    };
    (limits.warn().bytes(), limits.fail().bytes())
}

/// The SSH agent pipe or socket the settings in the data folder name, if any.
fn ssh_pipe(data: Option<&Path>) -> Option<String> {
    data.and_then(|data| ssh_agent_pipe(&FileSettings::new(HostPaths::new(data).settings())))
}

/// The report on `probe` as the text to print and the exit code; `disk_lines` are the free-space
/// lines in bytes (warn, fail).
pub(crate) fn report(
    probe: &dyn Probe,
    args: &DoctorArgs,
    disk_lines: (u64, u64),
) -> (String, ExitCode) {
    let options = Options {
        boot: !args.no_boot,
        disk_warn_below: disk_lines.0,
        disk_fail_below: disk_lines.1,
        ..Options::default()
    };
    let report = diagnose(probe, &options, puddle_types::VERSION);
    (render(&report, args.json), exit_code(&report))
}

fn render(report: &Report, json: bool) -> String {
    if json {
        format!("{}\n", report.to_json())
    } else {
        report.to_text()
    }
}

fn exit_code(report: &Report) -> ExitCode {
    if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests;
