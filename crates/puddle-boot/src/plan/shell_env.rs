// SPDX-License-Identifier: GPL-3.0-or-later
//! Carries the plan's environment past the places that drop it: `sudo` and shells that read no
//! login file.
//!
//! The environment reaches processes the agent starts through the sandbox's own environment, and
//! login shells through `ENV_FILE_GUEST`. Two paths lose it: `sudo` resets the environment to a
//! short list, and a terminal that starts a shell without a login (an editor's terminal, `su`,
//! `sudo -i` reaching a non-login `bash`) reads only the shell's own startup file. So the plan also
//! writes a `sudo` keep list and the same exports into the startup files of bash and zsh, in a
//! block between two markers (the files belong to the image, see [`puddle_types::MergeFormat`]).

use puddle_types::{GuestEnv, GuestFile, GuestPath, MergeEntry, MergeFormat, MergeSpec};

use super::{GENERATED, PlanError};
use crate::assets::guest_path;
use crate::quote::sh_word;

/// The `sudo` rule that keeps every variable of the plan.
pub const SUDOERS_ENV_GUEST: &str = "/etc/sudoers.d/puddle-env";

/// bash's system-wide startup file for interactive shells (Debian's name).
pub const BASH_RC_GUEST: &str = "/etc/bash.bashrc";

/// zsh's system-wide startup file, read by every zsh.
pub const ZSH_ENV_GUEST: &str = "/etc/zsh/zshenv";

const SUDOERS_MODE: u32 = 0o440;

/// The files for `env`: none when it is empty.
pub(super) fn files(env: &GuestEnv) -> Result<Vec<GuestFile>, PlanError> {
    if env.iter().next().is_none() {
        return Ok(Vec::new());
    }
    let names = env
        .iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>()
        .join(" ");
    let sudoers = GuestFile::new(
        guest_path(SUDOERS_ENV_GUEST),
        format!("{GENERATED}Defaults env_keep += \"{names}\"\n").into_bytes(),
    )
    .with_mode(SUDOERS_MODE)
    .map_err(|e| shell_error(&e))?;
    let exports = env
        .iter()
        .map(|(name, value)| format!("export {name}={}", sh_word(value)))
        .collect::<Vec<_>>()
        .join("\n");
    let spec = MergeSpec::new(
        MergeFormat::ShellBlock,
        vec![MergeEntry::shell_block(&exports)],
    )
    .map_err(|e| shell_error(&e))?;
    let merged = |path: &str| -> Result<GuestFile, PlanError> {
        let path = GuestPath::new(path).map_err(|e| shell_error(&e))?;
        Ok(GuestFile::merged(path, spec.clone()))
    };
    Ok(vec![
        sudoers,
        merged(BASH_RC_GUEST)?,
        merged(ZSH_ENV_GUEST)?,
    ])
}

fn shell_error(e: &puddle_types::ValidationError) -> PlanError {
    PlanError::ShellEnv {
        reason: e.to_string(),
    }
}
