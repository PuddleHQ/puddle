// SPDX-License-Identifier: AGPL-3.0-or-later
//! The msb runtime pair the VM tests run on, and the ambient settings that must not leak in.

use std::path::{Path, PathBuf};

use puddle_runtime::HostOs;

use crate::error::HarnessError;

/// msb environment variables that would override the harness's explicit runtime pair or home.
pub const AMBIENT_MSB_VARS: [&str; 5] = [
    "MSB_PATH",
    "MSB_LIBKRUNFW_PATH",
    "MSB_AGENTD_PATH",
    "MSB_HOME",
    "MSB_CONFIG_PATH",
];

/// Fails when any of [`AMBIENT_MSB_VARS`] is set according to `lookup`.
///
/// # Errors
///
/// [`HarnessError::AmbientMsbVar`] naming the first variable that is set.
pub fn refuse_ambient_msb_vars(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<(), HarnessError> {
    match AMBIENT_MSB_VARS
        .into_iter()
        .find(|var| lookup(var).is_some())
    {
        Some(var) => Err(HarnessError::AmbientMsbVar { var }),
        None => Ok(()),
    }
}

/// The `msb` executable and the `libkrunfw` library it was released with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimePair {
    /// `msb` (`msb.exe` on Windows).
    pub msb: PathBuf,
    /// `libkrunfw.so*`, `libkrunfw.dll` or `libkrunfw.dylib`.
    pub libkrunfw: PathBuf,
}

impl RuntimePair {
    /// Finds the pair for this OS in `dir` (msb's release archive layout: both files at the top).
    ///
    /// # Errors
    ///
    /// [`HarnessError::RuntimeIncomplete`] when a file is missing, [`HarnessError::Io`] when the
    /// directory can't be read, [`HarnessError::UnsupportedOs`] on an OS msb doesn't ship for.
    pub fn find_in(dir: &Path) -> Result<Self, HarnessError> {
        Self::find_for_os(dir, std::env::consts::OS)
    }

    fn find_for_os(dir: &Path, os: &'static str) -> Result<Self, HarnessError> {
        let Some(host) = HostOs::from_target_os(os) else {
            return Err(HarnessError::UnsupportedOs { os });
        };
        let files = host.runtime_files();
        let (msb_name, library_prefix) = (files.msb, files.libkrunfw_prefix);
        let msb = dir.join(msb_name);
        if !msb.is_file() {
            return Err(HarnessError::RuntimeIncomplete {
                dir: dir.to_owned(),
                missing: msb_name,
            });
        }
        let entries = std::fs::read_dir(dir).map_err(|e| HarnessError::io("read", dir, e))?;
        // Linux ships the versioned file (`libkrunfw.so.5.6.1`); take the shortest match so a
        // plain `libkrunfw.so` symlink wins when both exist.
        let libkrunfw = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(library_prefix))
            })
            .min_by_key(|path| path.as_os_str().len())
            .ok_or_else(|| HarnessError::RuntimeIncomplete {
                dir: dir.to_owned(),
                missing: "libkrunfw",
            })?;
        Ok(Self { msb, libkrunfw })
    }

    /// The msb `config.json` that pins this pair: the SDK reads runtime paths from it, and the
    /// sandbox runtimes' default `log_level` when one is given.
    #[must_use]
    pub fn config_json(&self, log_level: Option<&str>) -> String {
        let mut config = serde_json::Map::new();
        config.insert("version".into(), 1.into());
        config.insert(
            "paths".into(),
            serde_json::json!({ "msb": self.msb, "libkrunfw": self.libkrunfw }),
        );
        if let Some(level) = log_level {
            config.insert("log_level".into(), level.into());
        }
        serde_json::Value::Object(config).to_string()
    }
}

#[cfg(test)]
mod tests;
