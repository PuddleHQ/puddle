// SPDX-License-Identifier: AGPL-3.0-or-later
//! One Puddle host process per data folder.
//!
//! [`DataLock::acquire`] takes an exclusive advisory lock on a file in the data folder. The
//! operating system releases it when the holder exits for any reason, a crash or a kill
//! included, so a lock file left behind never blocks the next start (`flock` on Unix,
//! `LockFileEx` on Windows, both through [`File::try_lock`] in the standard library).
//!
//! The lock file stays empty because Windows locks are mandatory: a second process cannot read
//! a locked file. Who holds the lock is written to a second file, the holder note, which is
//! only a hint for the refusal message: it is written after the lock is taken and read only
//! when the lock is refused. In the instant between a new holder taking the lock and writing
//! its note, a refused process names the previous holder's process or none.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use crate::private;

/// The lock file's name in the data folder.
const LOCK_FILE: &str = "host.lock";
/// The holder note's name in the data folder.
const HOLDER_FILE: &str = "host.holder";

/// Why the data folder could not be locked.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another process holds the lock.
    #[error("{}", held_message(.dir, *.holder))]
    Held {
        /// The data folder.
        dir: PathBuf,
        /// The holder's process ID, when its note could be read.
        holder: Option<u32>,
    },
    /// The folder or the lock file could not be opened.
    #[error("cannot lock {path}: {source}")]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
}

fn held_message(dir: &Path, holder: Option<u32>) -> String {
    let who = holder.map_or_else(
        || "another Puddle process".to_owned(),
        |pid| format!("another Puddle process (process ID {pid})"),
    );
    format!(
        "{who} is already using the data folder {}; quit it first (a second one would stop \
         the first one's sandboxes)",
        dir.display()
    )
}

/// The exclusive hold on a data folder; released when dropped or when the process ends.
#[derive(Debug)]
pub struct DataLock {
    // Held for its lock: closing the file releases it.
    _file: File,
}

impl DataLock {
    /// Locks the data folder `dir` (created owner-only if missing) for this process.
    ///
    /// # Errors
    ///
    /// [`LockError::Held`] when another process holds it, [`LockError::Io`] when the folder or
    /// the lock file cannot be opened.
    pub fn acquire(dir: &Path) -> Result<Self, LockError> {
        let io_err = |path: &Path| {
            let path = path.to_path_buf();
            move |source| LockError::Io { path, source }
        };
        private::create_dir(dir).map_err(io_err(dir))?;
        let lock_path = dir.join(LOCK_FILE);
        let file = open_lock_file(&lock_path).map_err(io_err(&lock_path))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(LockError::Held {
                    dir: dir.to_path_buf(),
                    holder: read_holder(&dir.join(HOLDER_FILE)),
                });
            }
            Err(TryLockError::Error(source)) => {
                return Err(LockError::Io {
                    path: lock_path,
                    source,
                });
            }
        }
        // Only a hint for the next refusal, so a failure to write it never fails the start.
        let note = format!("{}\n", std::process::id());
        let _ = private::write_atomic(&dir.join(HOLDER_FILE), note.as_bytes());
        Ok(Self { _file: file })
    }
}

/// Opens the lock file, creating it owner-only the first time.
fn open_lock_file(path: &Path) -> io::Result<File> {
    match private::create_file(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            OpenOptions::new().read(true).write(true).open(path)
        }
        Err(e) => Err(e),
    }
}

fn read_holder(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests;
