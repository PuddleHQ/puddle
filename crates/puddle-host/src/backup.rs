// SPDX-License-Identifier: AGPL-3.0-or-later
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

use std::fs::{self, File};
use std::io::{self, Seek as _, SeekFrom};
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
    let cwd = std::env::current_dir().map_err(io_error("cannot read the working folder", dest))?;
    refuse_inside(&cwd, paths.data(), dest)?;
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

/// Refuses a destination that resolves into the data folder. A relative `dest` is read against
/// `cwd`; without that, a one-component name has no existing ancestor to resolve and would
/// skip the check.
fn refuse_inside(cwd: &Path, data: &Path, dest: &Path) -> Result<(), BackupError> {
    let data = data.canonicalize().unwrap_or_else(|_| data.to_path_buf());
    let absolute = cwd.join(dest);
    // The destination does not exist yet: resolve its deepest existing ancestor.
    let resolved = absolute.ancestors().find_map(|a| a.canonicalize().ok());
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

/// Makes backups of one data folder for the API: [`puddle_api::BackupService`] over
/// [`backup_archive`]. The copy runs on a blocking thread, since it reads and writes files and
/// waits for the database.
#[derive(Debug, Clone)]
pub(crate) struct DataFolderBackup {
    paths: HostPaths,
}

impl DataFolderBackup {
    pub(crate) fn new(paths: HostPaths) -> Self {
        Self { paths }
    }
}

impl puddle_api::BackupService for DataFolderBackup {
    fn create(
        &self,
    ) -> futures_util::future::BoxFuture<
        '_,
        Result<puddle_api::BackupArchive, puddle_api::BackupServiceError>,
    > {
        let paths = self.paths.clone();
        Box::pin(async move {
            let archive = blocking(move || backup_archive(&paths)).await?;
            Ok(puddle_api::BackupArchive::new(
                archive.size,
                tokio::fs::File::from_std(archive.file),
            ))
        })
    }
}

/// Runs `job` on a blocking thread; a job that fails or panics is a failed backup that says why.
async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, BackupError> + Send + 'static,
) -> Result<T, puddle_api::BackupServiceError> {
    let made = tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| puddle_api::BackupServiceError::Failed(format!("the backup stopped: {e}")))?;
    made.map_err(|e| puddle_api::BackupServiceError::Failed(e.to_string()))
}

/// A backup packed into one file: a gzip-compressed tar archive of the three parts at its top
/// level (`puddle.db`, `settings/…`, `workspaces.json`), ready to read from the start.
#[derive(Debug)]
pub struct BackupArchive {
    /// The archive, open and positioned at its first byte. It has no name on disk: it is gone when
    /// the last handle closes, so a half-sent download leaves nothing behind.
    pub file: File,
    /// The archive's size in bytes.
    pub size: u64,
    /// What the backup inside holds.
    pub report: BackupReport,
}

/// Makes a backup of the data folder `paths` names (see [`backup`]) and packs it into one archive.
///
/// The parts are staged in a private temporary folder that is removed before this returns, and the
/// archive is an unnamed temporary file; nothing is written in the data folder.
///
/// # Errors
///
/// [`BackupError`] as for [`backup`], or [`BackupError::Io`] when the archive cannot be written.
pub fn backup_archive(paths: &HostPaths) -> Result<BackupArchive, BackupError> {
    backup_archive_in(paths, &tempfile::env::temp_dir())
}

/// [`backup_archive`] with its temporary folder and file made in `scratch`.
fn backup_archive_in(paths: &HostPaths, scratch: &Path) -> Result<BackupArchive, BackupError> {
    let staging = tempfile::Builder::new()
        .prefix("puddle-backup-")
        .tempdir_in(scratch)
        .map_err(io_error("cannot create a temporary folder in", scratch))?;
    let folder = staging.path().join("backup");
    let report = backup(paths, &folder)?;
    let (file, size) = pack(&folder, scratch)?;
    Ok(BackupArchive { file, size, report })
}

/// Packs the backup folder `folder` into a new unnamed temporary file made in `scratch`, with no
/// owner, group or modification time in the entries, so the same backup always packs to the same
/// entries.
fn pack(folder: &Path, scratch: &Path) -> Result<(File, u64), BackupError> {
    pack_io(folder, scratch).map_err(io_error("cannot write the archive of", folder))
}

fn pack_io(folder: &Path, scratch: &Path) -> io::Result<(File, u64)> {
    let file = tempfile::tempfile_in(scratch)?;
    let gzip = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(gzip);
    tar.mode(tar::HeaderMode::Deterministic);
    tar.follow_symlinks(false);
    append_below(&mut tar, folder, Path::new(""))?;
    let mut file = tar.into_inner()?.finish()?;
    let size = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(0))?;
    Ok((file, size))
}

/// Appends the files below `folder/relative`, in name order so the archive is stable. A backup
/// folder holds only regular files and folders; anything else would be written as the link or
/// special entry it is, which a restore refuses.
fn append_below<W: io::Write>(
    tar: &mut tar::Builder<W>,
    folder: &Path,
    relative: &Path,
) -> io::Result<()> {
    let mut entries = fs::read_dir(folder.join(relative))?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = relative.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            append_below(tar, folder, &name)?;
        } else {
            tar.append_path_with_name(entry.path(), &name)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
