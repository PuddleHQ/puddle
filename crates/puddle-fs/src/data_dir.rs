// SPDX-License-Identifier: AGPL-3.0-or-later
//! Puddle's per-user data folder.

use std::path::{Path, PathBuf};

/// The folder's name inside the OS's per-user data location.
const APP_DIR_NAME: &str = "puddle";

/// Why there is no data folder.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataDirError {
    /// The OS reports no per-user data location (no home folder).
    #[error(
        "cannot find the per-user data folder: the OS reports no home or local app data folder"
    )]
    NoBase,
    /// The location isn't absolute (a relative `XDG_DATA_HOME` is ignored by `dirs`, so this is a
    /// guard for other platforms): msb would resolve it against whatever the working directory is.
    #[error("the per-user data folder must be absolute: {path}")]
    Relative {
        /// The offending path.
        path: PathBuf,
    },
}

/// Puddle's per-user data folder for this OS (see the crate documentation for the table). The
/// folder isn't created.
///
/// # Errors
///
/// [`DataDirError::NoBase`] without a home folder, [`DataDirError::Relative`] if the OS gave a
/// relative path.
pub fn data_dir() -> Result<PathBuf, DataDirError> {
    data_dir_in(dirs::data_local_dir().as_deref())
}

/// [`data_dir`] from an explicit per-user data location (`None` when the OS has none); the
/// testable half.
///
/// # Errors
///
/// As [`data_dir`].
pub fn data_dir_in(base: Option<&Path>) -> Result<PathBuf, DataDirError> {
    let base = base.ok_or(DataDirError::NoBase)?;
    if !base.is_absolute() {
        return Err(DataDirError::Relative {
            path: base.to_path_buf(),
        });
    }
    Ok(base.join(APP_DIR_NAME))
}

/// The user's own ssh config, `~/.ssh/config` (not created), or `None` without a home folder.
#[must_use]
pub fn user_ssh_config() -> Option<PathBuf> {
    user_ssh_config_in(dirs::home_dir().as_deref())
}

/// [`user_ssh_config`] from an explicit home folder; a relative one is no home.
#[must_use]
pub fn user_ssh_config_in(home: Option<&Path>) -> Option<PathBuf> {
    let home = home.filter(|h| h.is_absolute())?;
    Some(home.join(".ssh").join("config"))
}

#[cfg(test)]
mod tests;
