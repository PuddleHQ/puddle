// SPDX-License-Identifier: AGPL-3.0-or-later
//! One Credential Manager call at a time across every Puddle process of the user.
//!
//! Credential Manager loses changes when several processes change it at the same time. Four
//! processes that each wrote 30 entries and deleted them again, side by side, left 29 to 43 of the
//! deleted entries behind, and some of them read an entry back right after their own delete of it
//! had succeeded; the same entries written and deleted by one process alone, or by the four
//! processes with a shared lock around each call, left none. A host, `puddle secrets` and
//! `puddle doctor` (which writes a probe entry) can run at once, and so can test processes.
//!
//! So every call [`super::KeyringStore`] makes holds an exclusive lock on one file in the user's
//! default Puddle data folder, which every Puddle process of the user finds whatever data folder
//! it was given. Changes other programs make at the same moment are not covered. This is how
//! Credential Manager behaves, not a bug in a crate we could fix, so the lock is meant to stay. A
//! call that never returns would hold every other Puddle process's calls; Credential Manager calls
//! do not wait for the user, and the operating system drops the lock when its holder exits.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The lock file's name in the default data folder.
const LOCK_FILE: &str = "credential-store.lock";

/// The exclusive hold on the lock file; released when dropped.
pub(super) struct CredentialLock {
    file: File,
}

impl Drop for CredentialLock {
    fn drop(&mut self) {
        // Closing the file releases the lock too; the unlock only makes it immediate.
        let _ = self.file.unlock();
    }
}

impl CredentialLock {
    /// Waits for the lock. `None` when the lock file cannot be opened or locked: the call then
    /// goes ahead without it, because a store that answers is worth more than the order.
    pub(super) fn hold() -> Option<Self> {
        let locked = lock_path().and_then(|path| {
            let file = open(&path)?;
            file.lock()?;
            Ok(file)
        });
        match locked {
            Ok(file) => Some(Self { file }),
            Err(err) => {
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        %err,
                        "the credential store lock could not be taken; calls go ahead without it"
                    );
                }
                None
            }
        }
    }
}

fn lock_path() -> io::Result<PathBuf> {
    let dir = puddle_fs::data_dir().map_err(io::Error::other)?;
    puddle_fs::private::create_dir(&dir)?;
    Ok(dir.join(LOCK_FILE))
}

/// Opens the lock file, creating it owner-only the first time.
fn open(path: &Path) -> io::Result<File> {
    match puddle_fs::private::create_file(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            OpenOptions::new().read(true).write(true).open(path)
        }
        Err(e) => Err(e),
    }
}
