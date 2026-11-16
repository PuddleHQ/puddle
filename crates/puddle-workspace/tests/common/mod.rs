// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared by the tests that run `git` (or scripts that do) on the host.
//!
//! Under a git hook (the pre-push gates run the tests) git exports `GIT_DIR`, `GIT_INDEX_FILE`
//! and friends. Every `git` the tests spawn would then act on Puddle's own repository whatever
//! its working directory is: `git init --bare` in a temp dir rewrites that repository's
//! `core.bare`. [`command`] and [`git_command`] drop every inherited `GIT_*` variable.

use std::ffi::OsString;
use std::process::Command;

/// `program` with every `GIT_*` variable in `inherited` removed.
pub(crate) fn command_without(
    program: &str,
    inherited: impl IntoIterator<Item = OsString>,
) -> Command {
    let mut cmd = Command::new(program);
    for key in inherited {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd
}

/// `program` without the caller's `GIT_*` variables.
pub(crate) fn command(program: &str) -> Command {
    command_without(program, std::env::vars_os().map(|(key, _)| key))
}

/// `git` without the caller's `GIT_*` variables.
pub(crate) fn git_command() -> Command {
    command("git")
}

#[cfg(test)]
mod tests;
