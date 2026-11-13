// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where the bundled runtime and Puddle's private msb home live.

use std::path::{Path, PathBuf};

use crate::RuntimeError;
use crate::platform::HostOs;

/// The runtime folder's name, next to the Puddle executable.
pub const RUNTIME_DIR_NAME: &str = "runtime";

/// The msb executable's file name on this platform (the [`HostOs::runtime_files`] table).
pub const MSB_FILE_NAME: &str = HostOs::current().runtime_files().msb;

/// The firmware library's file name on this platform: what msb loads from beside its own binary on
/// Windows (msb's `libkrunfw_filename`), and the SONAME-named file on Linux, where the runtime
/// folder also holds a copy under the versioned name msb looks for
/// ([`HostOs::libkrunfw_versioned`]; [`RuntimeLayout::required_files`] doesn't list it).
pub const LIBKRUNFW_FILE_NAME: &str = HostOs::current().runtime_files().libkrunfw;

/// The msb home's name inside Puddle's data folder.
const HOME_DIR_NAME: &str = "msb";

/// The config file inside the msb home (msb's own default name, so a plain `msb` pointed at this
/// home reads the same file).
const CONFIG_FILE_NAME: &str = "config.json";

/// The two folders Puddle's runtime uses, both absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLayout {
    runtime_dir: PathBuf,
    home: PathBuf,
}

impl RuntimeLayout {
    /// A layout from explicit folders: `runtime_dir` holds `msb(.exe)` and its libraries, `home`
    /// becomes `MSB_HOME`.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::RelativePath`] if either isn't absolute: msb would resolve it against
    /// whatever directory Puddle happens to run in.
    pub fn new(runtime_dir: PathBuf, home: PathBuf) -> Result<Self, RuntimeError> {
        for path in [&runtime_dir, &home] {
            if !path.is_absolute() {
                return Err(RuntimeError::RelativePath { path: path.clone() });
            }
        }
        Ok(Self { runtime_dir, home })
    }

    /// The installed layout: the runtime in `<folder of exe>/runtime`, the home in
    /// `<data_dir>/msb`. `exe` is normally [`std::env::current_exe`]; `data_dir` is Puddle's
    /// per-user data folder ([`puddle_fs::data_dir`]; [`RuntimeLayout::installed_for_user`]
    /// resolves it).
    ///
    /// # Errors
    ///
    /// [`RuntimeError::NoExeDir`] if `exe` has no parent, [`RuntimeError::RelativePath`] if a
    /// resulting folder isn't absolute.
    pub fn installed(exe: &Path, data_dir: &Path) -> Result<Self, RuntimeError> {
        let exe_dir = exe.parent().ok_or_else(|| RuntimeError::NoExeDir {
            path: exe.to_path_buf(),
        })?;
        Self::new(exe_dir.join(RUNTIME_DIR_NAME), data_dir.join(HOME_DIR_NAME))
    }

    /// The installed layout with the data folder from [`puddle_fs::data_dir`].
    ///
    /// # Errors
    ///
    /// [`RuntimeError::NoDataDir`] without a per-user data folder, else as
    /// [`RuntimeLayout::installed`].
    pub fn installed_for_user(exe: &Path) -> Result<Self, RuntimeError> {
        let data = puddle_fs::data_dir().map_err(|e| RuntimeError::NoDataDir {
            reason: e.to_string(),
        })?;
        Self::installed(exe, &data)
    }

    /// The runtime folder.
    #[must_use]
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    /// Puddle's private msb home (`MSB_HOME`).
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The bundled msb executable (`MSB_PATH`).
    #[must_use]
    pub fn msb_path(&self) -> PathBuf {
        self.runtime_dir.join(MSB_FILE_NAME)
    }

    /// The firmware library beside msb ([`LIBKRUNFW_FILE_NAME`]).
    #[must_use]
    pub fn libkrunfw_path(&self) -> PathBuf {
        self.runtime_dir.join(LIBKRUNFW_FILE_NAME)
    }

    /// The files that must exist for the runtime to start: `msb`, plus the firmware where the
    /// check can name it (Windows, [`crate::RuntimeFiles::firmware_beside_msb`]), so
    /// `MSB_LIBKRUNFW_PATH` stays unset. On Linux the versioned copy is checked by msb itself.
    #[must_use]
    pub fn required_files(&self) -> Vec<PathBuf> {
        let mut files = vec![self.msb_path()];
        if HostOs::current().runtime_files().firmware_beside_msb {
            files.push(self.libkrunfw_path());
        }
        files
    }

    /// msb's config file inside Puddle's home (`MSB_CONFIG_PATH`); without it msb falls back to
    /// `~/.microsandbox/config.json`.
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.home.join(CONFIG_FILE_NAME)
    }
}

#[cfg(test)]
mod tests;
