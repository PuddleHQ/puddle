// SPDX-License-Identifier: AGPL-3.0-or-later
//! The server half of Microsoft's secret-storage key, one per workspace, kept in a file only the
//! user can read.
//!
//! It lives on the host, not on the workspace volume, so reading the guest's disk does not give it
//! away. A hostile guest's own served page is same-origin with the front and can still ask for the
//! key, so this protects stored secrets against the volume, not against a compromised editor page. It outlives a rebuilt sandbox, as the
//! volume does, and goes with the workspace ([`ServerHalves::forget`]).

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use puddle_types::WorkspaceId;

/// Bytes in a half.
pub const HALF_LEN: usize = 32;

/// Where the server halves are kept.
pub trait ServerHalves: Send + Sync {
    /// The workspace's half, made and stored the first time it is asked for.
    ///
    /// # Errors
    ///
    /// The half could not be read, made or stored.
    fn half(&self, workspace: &WorkspaceId) -> io::Result<[u8; HALF_LEN]>;

    /// Deletes the workspace's half, for good: a workspace made later under the same name gets a
    /// new one. Idempotent.
    ///
    /// # Errors
    ///
    /// The file exists and could not be deleted.
    fn forget(&self, workspace: &WorkspaceId) -> io::Result<()>;

    /// Whether a half is stored for `workspace`. One that cannot be looked for counts as there.
    fn has(&self, workspace: &WorkspaceId) -> bool;
}

/// [`ServerHalves`] in owner-only files, `<folder>/<workspace>.key`, hex.
#[derive(Debug)]
pub struct FileHalves {
    dir: PathBuf,
    cache: Mutex<HashMap<WorkspaceId, [u8; HALF_LEN]>>,
}

impl FileHalves {
    /// Halves kept in `dir`, which is made owner-only when the first one is stored.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn path(&self, workspace: &WorkspaceId) -> PathBuf {
        self.dir.join(format!("{workspace}.key"))
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}"); // writing to a String cannot fail
        s
    })
}

fn unhex(text: &str) -> Option<[u8; HALF_LEN]> {
    let text = text.trim();
    if text.len() != HALF_LEN * 2 || !text.is_ascii() {
        return None;
    }
    let mut out = [0_u8; HALF_LEN];
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

impl ServerHalves for FileHalves {
    fn half(&self, workspace: &WorkspaceId) -> io::Result<[u8; HALF_LEN]> {
        // The lock is held across the file work on purpose: two first requests must not each make
        // a half and keep different ones. The files are 64 bytes.
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(half) = cache.get(workspace) {
            return Ok(*half);
        }
        let path = self.path(workspace);
        let stored = match std::fs::read_to_string(&path) {
            Ok(text) => unhex(&text),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        let half = if let Some(half) = stored {
            half
        } else {
            // A file that is there and not a half is replaced: secrets stored under it are lost
            // either way, and the editor asks its user to sign in again.
            let mut half = [0_u8; HALF_LEN];
            getrandom::fill(&mut half)
                .map_err(|e| io::Error::other(format!("no randomness: {e}")))?;
            puddle_fs::private::write_atomic(&path, hex(&half).as_bytes())?;
            half
        };
        cache.insert(workspace.clone(), half);
        Ok(half)
    }

    fn forget(&self, workspace: &WorkspaceId) -> io::Result<()> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.remove(workspace);
        match std::fs::remove_file(self.path(workspace)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }

    fn has(&self, workspace: &WorkspaceId) -> bool {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.contains_key(workspace) || !matches!(self.path(workspace).try_exists(), Ok(false))
    }
}

#[cfg(test)]
mod tests;
