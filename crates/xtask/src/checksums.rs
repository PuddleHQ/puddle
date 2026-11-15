// SPDX-License-Identifier: AGPL-3.0-or-later
//! SHA-256 checksums: hashing files and checking them against a release's `checksums.sha256`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Result, XtaskError};

/// The lowercase hex SHA-256 of the file at `path`, read in chunks.
///
/// # Errors
///
/// [`XtaskError::Io`] when the file can't be read.
pub fn sha256_file(path: &Path) -> Result<String> {
    let context = || format!("hashing {}", path.display());
    let mut file = std::fs::File::open(path).map_err(|e| XtaskError::io(context(), e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| XtaskError::io(context(), e))?;
        let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
            break;
        };
        hasher.update(chunk);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}"); // writing to a String can't fail
        s
    })
}

/// A parsed `checksums.sha256` (`sha256sum` format: `<64 hex>  <name>`, `*<name>` for binary
/// mode), keyed by file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checksums {
    list: String,
    sums: BTreeMap<String, String>,
}

impl Checksums {
    /// Parses `text`; `list` names it in errors (e.g. `fork v0.7.7-puddle.1 checksums.sha256`).
    ///
    /// # Errors
    ///
    /// [`XtaskError::Parse`] for a malformed line or a file listed twice.
    pub fn parse(list: &str, text: &str) -> Result<Self> {
        let mut sums = BTreeMap::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            let bad = |why: &str| XtaskError::parse(list, format!("line {}: {why}", n + 1));
            let (sum, name) = line
                .split_once(char::is_whitespace)
                .ok_or_else(|| bad("no file name"))?;
            let name = name.trim_start();
            let name = name.strip_prefix('*').unwrap_or(name);
            if sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(bad("not a SHA-256"));
            }
            if name.is_empty() {
                return Err(bad("no file name"));
            }
            if sums
                .insert(name.to_owned(), sum.to_ascii_lowercase())
                .is_some()
            {
                return Err(bad("file listed twice"));
            }
        }
        Ok(Self {
            list: list.to_owned(),
            sums,
        })
    }

    /// Checks that the file at `path` has the checksum listed for `name`; returns it.
    ///
    /// # Errors
    ///
    /// [`XtaskError::NotListed`], [`XtaskError::Checksum`], or [`XtaskError::Io`].
    pub fn verify(&self, name: &str, path: &Path) -> Result<String> {
        let expected = self.sums.get(name).ok_or_else(|| XtaskError::NotListed {
            file: name.to_owned(),
            list: self.list.clone(),
        })?;
        let actual = sha256_file(path)?;
        if &actual == expected {
            Ok(actual)
        } else {
            Err(XtaskError::Checksum {
                file: name.to_owned(),
                list: self.list.clone(),
                expected: expected.clone(),
                actual,
            })
        }
    }
}

#[cfg(test)]
mod tests;
