// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle backup`: a consistent copy of the state a restore needs, made while the host runs.
//!
//! The copy holds the database (`puddle.db`: rules, pending requests, audit log, environment
//! rows, git identities), the settings documents (`settings/`) and the workspace list
//! (`workspaces.json`). The database copy has the users but not their password hashes or open
//! invites ([`puddle_store::backup_database`]). It never holds what only makes sense on this machine or is secret: the
//! credential store's contents live in the operating system and are not read; the login markers
//! and the browser front's key halves, the connection file and the lock files stay behind; and
//! the sandbox runtime and the workspace volumes are too large and have their own owner.
//!
//! This process never takes the data-folder lock, since the host that runs holds it, and it
//! writes nothing in the data folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use puddle_fs::private;

use crate::HostPaths;

/// The database's file name inside a backup.
pub const BACKUP_DATABASE: &str = "puddle.db";
/// The settings folder's name inside a backup.
pub const BACKUP_SETTINGS: &str = "settings";
/// The workspace list's file name inside a backup.
pub const BACKUP_WORKSPACES: &str = "workspaces.json";

/// What a finished backup holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReport {
    /// The folder the backup is in.
    pub dest: PathBuf,
    /// The database's schema version; restore it with a Puddle that knows it.
    pub schema_version: u32,
    /// How many settings files were copied.
    pub settings_files: usize,
    /// Whether the data folder had a workspace list to copy.
    pub workspace_list: bool,
}

/// Why a backup was not made. Messages name paths, never contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BackupError {
    /// The destination already exists; a backup never writes into a folder that is there.
    #[error("{0} already exists; choose a new folder")]
    DestinationExists(PathBuf),
    /// The destination is inside the data folder, which would make the backup part of the
    /// next one.
    #[error("{0} is inside Puddle's data folder; choose a folder outside it")]
    InsideData(PathBuf),
    /// There is no database to copy.
    #[error("no database at {0}; is this the right data folder?")]
    NoDatabase(PathBuf),
    /// The database could not be copied or failed its check.
    #[error("cannot copy the database: {0}")]
    Database(#[from] puddle_store::StoreError),
    /// A file or folder could not be read or written.
    #[error("{what}: {source}")]
    Io {
        /// What was being done, with the path.
        what: String,
        /// What went wrong.
        source: io::Error,
    },
}

fn io_error(what: &str, path: &Path) -> impl FnOnce(io::Error) -> BackupError {
    let what = format!("{what} {}", path.display());
    move |source| BackupError::Io { what, source }
}

/// Makes a backup of the data folder `paths` names in the new folder `dest`, which is created
/// owner-only (its parent is created if missing). On failure the folder is removed again, so a
/// folder that exists is a complete backup.
///
/// # Errors
///
/// [`BackupError`] as its variants say.
pub fn backup(paths: &HostPaths, dest: &Path) -> Result<BackupReport, BackupError> {
    let source_db = paths.store();
    if !source_db.is_file() {
        return Err(BackupError::NoDatabase(source_db));
    }
    if dest.exists() {
        return Err(BackupError::DestinationExists(dest.to_path_buf()));
    }
    refuse_inside(paths.data(), dest)?;
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        private::create_dir(parent).map_err(io_error("cannot create", parent))?;
    }
    private::create_new_dir(dest).map_err(io_error("cannot create", dest))?;
    match fill(paths, dest) {
        Ok(report) => Ok(report),
        Err(err) => {
            // The folder is ours (create_new_dir made it a moment ago), so it is safe to remove.
            // Best effort: the failure to report is the backup's, not the cleanup's.
            drop(fs::remove_dir_all(dest));
            Err(err)
        }
    }
}

fn fill(paths: &HostPaths, dest: &Path) -> Result<BackupReport, BackupError> {
    let db = dest.join(BACKUP_DATABASE);
    let info = puddle_store::backup_database(&paths.store(), &db)?;
    // VACUUM INTO makes the file with the process's default mode; the folder is owner-only, and
    // the file is tightened too so a later move out of the folder keeps it private.
    private::tighten_file(&db).map_err(io_error("cannot restrict", &db))?;

    let settings_files = copy_tree(&paths.settings(), &dest.join(BACKUP_SETTINGS))?;
    let workspace_list = copy_file(&paths.workspace_book(), &dest.join(BACKUP_WORKSPACES))?;
    Ok(BackupReport {
        dest: dest.to_path_buf(),
        schema_version: info.schema_version,
        settings_files,
        workspace_list,
    })
}

/// Refuses a destination that resolves into the data folder.
fn refuse_inside(data: &Path, dest: &Path) -> Result<(), BackupError> {
    let data = data.canonicalize().unwrap_or_else(|_| data.to_path_buf());
    // The destination does not exist yet: resolve its deepest existing ancestor.
    let resolved = dest.ancestors().find_map(|a| a.canonicalize().ok());
    if resolved.is_some_and(|real| real.starts_with(&data)) {
        return Err(BackupError::InsideData(dest.to_path_buf()));
    }
    Ok(())
}

/// Copies one regular file; `false` when it does not exist. Puddle writes these files
/// atomically (a rename), so a copy sees the old or the new file, never half of one.
fn copy_file(from: &Path, to: &Path) -> Result<bool, BackupError> {
    let body = match fs::read(from) {
        Ok(body) => body,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(io_error("cannot read", from)(e)),
    };
    private::write_atomic(to, &body).map_err(io_error("cannot write", to))?;
    Ok(true)
}

/// Copies the regular files below `from` to `to`, owner-only, skipping links and the
/// temporary files of an atomic write in progress; the number copied. No folder is no files.
fn copy_tree(from: &Path, to: &Path) -> Result<usize, BackupError> {
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(io_error("cannot read", from)(e)),
    };
    private::create_dir(to).map_err(io_error("cannot create", to))?;
    let mut copied = 0;
    for entry in entries {
        let entry = entry.map_err(io_error("cannot read", from))?;
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".tmp") {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(io_error("cannot read", &entry.path()))?;
        if kind.is_dir() {
            copied += copy_tree(&entry.path(), &to.join(&name))?;
        } else if kind.is_file() && copy_file(&entry.path(), &to.join(&name))? {
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests;
