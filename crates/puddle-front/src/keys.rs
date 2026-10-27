// SPDX-License-Identifier: GPL-3.0-or-later
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

use puddle_types::WorkspaceName;

/// Bytes in a half.
pub const HALF_LEN: usize = 32;

/// Where the server halves are kept.
pub trait ServerHalves: Send + Sync {
    /// The workspace's half, made and stored the first time it is asked for.
    ///
    /// # Errors
    ///
    /// The half could not be read, made or stored.
    fn half(&self, workspace: &WorkspaceName) -> io::Result<[u8; HALF_LEN]>;

    /// Deletes the workspace's half, for good: a workspace made later under the same name gets a
    /// new one. Idempotent.
    ///
    /// # Errors
    ///
    /// The file exists and could not be deleted.
    fn forget(&self, workspace: &WorkspaceName) -> io::Result<()>;
}

/// [`ServerHalves`] in owner-only files, `<folder>/<workspace>.key`, hex.
#[derive(Debug)]
pub struct FileHalves {
    dir: PathBuf,
    cache: Mutex<HashMap<WorkspaceName, [u8; HALF_LEN]>>,
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

    fn path(&self, workspace: &WorkspaceName) -> PathBuf {
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
    fn half(&self, workspace: &WorkspaceName) -> io::Result<[u8; HALF_LEN]> {
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

    fn forget(&self, workspace: &WorkspaceName) -> io::Result<()> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.remove(workspace);
        match std::fs::remove_file(self.path(workspace)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> WorkspaceName {
        WorkspaceName::new(n).unwrap()
    }

    #[test]
    fn a_half_is_made_once_kept_and_read_back_by_a_new_store() {
        let dir = tempfile::tempdir().unwrap();
        let first = FileHalves::new(dir.path().join("front"));
        let a = first.half(&name("a")).unwrap();
        assert_eq!(first.half(&name("a")).unwrap(), a);
        assert_ne!(first.half(&name("b")).unwrap(), a);
        let second = FileHalves::new(dir.path().join("front"));
        assert_eq!(second.half(&name("a")).unwrap(), a);
    }

    #[test]
    fn forgetting_a_half_gives_the_name_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let halves = FileHalves::new(dir.path());
        let old = halves.half(&name("a")).unwrap();
        halves.forget(&name("a")).unwrap();
        halves.forget(&name("a")).unwrap();
        assert_ne!(halves.half(&name("a")).unwrap(), old);
    }

    #[test]
    fn a_file_that_is_not_a_half_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.key"), "not hex").unwrap();
        let halves = FileHalves::new(dir.path());
        let half = halves.half(&name("a")).unwrap();
        let again = FileHalves::new(dir.path()).half(&name("a")).unwrap();
        assert_eq!(half, again);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_for_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        FileHalves::new(dir.path().join("front"))
            .half(&name("a"))
            .unwrap();
        let mode = std::fs::metadata(dir.path().join("front/a.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    #[test]
    fn hex_round_trips_and_refuses_the_wrong_length() {
        let bytes: Vec<u8> = (0..32).collect();
        assert_eq!(unhex(&hex(&bytes)).unwrap().to_vec(), bytes);
        assert!(unhex("abcd").is_none());
        assert!(unhex(&"zz".repeat(32)).is_none());
        assert!(unhex(&"é".repeat(32)).is_none());
    }
}
