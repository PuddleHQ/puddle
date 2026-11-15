// SPDX-License-Identifier: AGPL-3.0-or-later
//! The environment that makes the msb SDK use Puddle's runtime and home, and nothing of the user's.
//!
//! The SDK takes Puddle's runtime and home as explicit arguments and ignores the environment
//! (`puddle-compute-msb`, `LocalBackendBuilder::ignore_environment`), so this is only for the
//! processes Puddle starts itself: msb reads `MSB_BACKEND`, `MSB_API_URL`, `MSB_PROFILE` and
//! more, any of which could redirect Puddle. The rule is simple: remove every `MSB_*` variable the
//! child would inherit, then set exactly the three Puddle owns.

use std::ffi::{OsStr, OsString};
use std::process::Command;

use crate::RuntimeLayout;

/// Whether `name` is one of msb's variables: starts with `MSB_`, ASCII case-insensitively
/// (Windows environment names are case-insensitive, so `msb_path` counts too).
#[must_use]
pub fn is_msb_variable(name: &OsStr) -> bool {
    name.as_encoded_bytes()
        .get(..4)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"MSB_"))
}

/// The environment changes for the bundled runtime: variables to remove, then variables to set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEnv {
    remove: Vec<OsString>,
    set: Vec<(&'static str, OsString)>,
}

impl RuntimeEnv {
    /// The plan for `layout`, given the current environment `vars` (normally
    /// [`std::env::vars_os`]): every `MSB_*` name in `vars` is removed, then `MSB_PATH`,
    /// `MSB_HOME` and `MSB_CONFIG_PATH` are set to the layout's absolute paths.
    #[must_use]
    pub fn plan(
        layout: &RuntimeLayout,
        vars: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Self {
        let mut remove: Vec<OsString> = vars
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| is_msb_variable(name))
            .collect();
        remove.sort();
        remove.dedup();
        let set = vec![
            ("MSB_PATH", layout.msb_path().into_os_string()),
            ("MSB_HOME", layout.home().as_os_str().to_owned()),
            ("MSB_CONFIG_PATH", layout.config_path().into_os_string()),
        ];
        Self { remove, set }
    }

    /// The variables removed (each user-set `MSB_*` name, sorted).
    #[must_use]
    pub fn removed(&self) -> &[OsString] {
        &self.remove
    }

    /// The variables set, in order: `MSB_PATH`, `MSB_HOME`, `MSB_CONFIG_PATH`.
    #[must_use]
    pub fn set(&self) -> &[(&'static str, OsString)] {
        &self.set
    }

    /// Applies the plan to a child process: removes the user's `MSB_*` variables from what the
    /// child inherits and sets Puddle's.
    pub fn apply_to(&self, command: &mut Command) {
        for name in &self.remove {
            command.env_remove(name);
        }
        for (name, value) in &self.set {
            command.env(name, value);
        }
    }
}

#[cfg(test)]
mod tests;
