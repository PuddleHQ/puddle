// SPDX-License-Identifier: GPL-3.0-or-later
//! Which data folder `puddle backup` and `puddle doctor` look at: the one `puddle serve` would use.
//!
//! Serve reads `data_dir` from its option table (flag, `PUDDLE_DATA_DIR`, the `--config` file, then
//! the per-user folder). These commands take the same two options, `--data-dir` and `--config`, and
//! resolve them through the same table, so a host started with an environment variable or a config
//! file is the one a backup copies and the doctor checks.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::cli::UsageError;
use crate::cmd::serve::config_file::ConfigError;
use crate::cmd::serve::options::{Flags, resolve, spec_for_flag};

/// The option keys these commands share with serve.
const KEYS: [&str; 2] = ["data_dir", "config"];

/// Takes `arg` as `--data-dir <path>` or `--config <path>` into `flags` (the value comes from
/// `rest`) and says whether it was one of them.
///
/// # Errors
///
/// [`UsageError::MissingArgument`] without a value, [`UsageError::UnknownArgument`] for a
/// repeated option.
pub fn take_option<I, S>(arg: &str, rest: &mut I, flags: &mut Flags) -> Result<bool, UsageError>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let Some(spec) = spec_for_flag(arg).filter(|spec| KEYS.contains(&spec.key)) else {
        return Ok(false);
    };
    if flags.contains_key(spec.key) {
        return Err(UsageError::UnknownArgument(arg.to_owned()));
    }
    let text = rest.next().ok_or(UsageError::MissingArgument(spec.usage))?;
    let value =
        spec.kind
            .parse_text(text.as_ref())
            .map_err(|expected| UsageError::InvalidValue {
                option: spec.flag(),
                expected,
                value: text.as_ref().to_owned(),
            })?;
    flags.insert(spec.key, value);
    Ok(true)
}

/// The data folder the options name, or `None` when none does (the per-user folder then).
/// `env` reads an environment variable; `cwd` is what a relative path means.
///
/// # Errors
///
/// [`ConfigError`] when an environment variable or the config file is not usable.
pub fn configured(
    flags: &Flags,
    env: &dyn Fn(&str) -> Option<OsString>,
    cwd: &Path,
) -> Result<Option<PathBuf>, ConfigError> {
    Ok(resolve(flags, env, cwd)?
        .path("data_dir")
        .map(Path::to_path_buf))
}

/// [`configured`] for this process's environment and working folder.
///
/// # Errors
///
/// [`ConfigError`] as [`configured`] says, or when the working folder is unknown.
pub fn configured_here(flags: &Flags) -> Result<Option<PathBuf>, ConfigError> {
    let cwd = std::env::current_dir().map_err(|e| ConfigError::WorkingDirectory(e.to_string()))?;
    configured(flags, &|name| std::env::var_os(name), &cwd)
}

#[cfg(test)]
mod tests;
