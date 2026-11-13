// SPDX-License-Identifier: AGPL-3.0-or-later
//! The API's bearer token, and the user-only connection file that hands it (with the URL) to the
//! CLI and the UI.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

/// Random bytes in a token (256 bits).
const TOKEN_BYTES: usize = 32;

/// Largest connection file read back; a real one is under 200 bytes.
const MAX_FILE_BYTES: u64 = 4096;

/// Version of the connection file's format.
const FILE_VERSION: u32 = 1;

/// The API's bearer token: 32 random bytes as 64 lower-case hex characters.
///
/// `Debug` never shows the value and there is no `Display`; [`ApiToken::expose`] is the one way
/// to read it, for the connection file and for clients that send it. The text is overwritten when
/// the value drops.
#[derive(Clone)]
pub struct ApiToken(String);

impl ApiToken {
    /// A fresh token from the OS random source.
    ///
    /// # Errors
    ///
    /// [`ConnectionFileError::Random`] if the OS has no randomness to give.
    pub fn generate() -> Result<Self, ConnectionFileError> {
        let mut bytes = Zeroizing::new([0u8; TOKEN_BYTES]);
        getrandom::fill(&mut *bytes).map_err(|err| ConnectionFileError::Random(err.to_string()))?;
        Ok(Self(hex(&*bytes)))
    }

    /// Whether `presented` is this token, compared in constant time. A different length is
    /// refused at once (the length isn't secret).
    #[must_use]
    pub fn matches(&self, presented: &[u8]) -> bool {
        self.0.as_bytes().ct_eq(presented).into()
    }

    /// The token text, to write into the connection file or send in a request.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn parse(text: String) -> Result<Self, String> {
        // A malformed text is dropped on the error path; this wipes it there too.
        let mut text = Zeroizing::new(text);
        let well_formed = text.len() == TOKEN_BYTES * 2
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if well_formed {
            Ok(Self(std::mem::take(&mut *text)))
        } else {
            Err("token is not 64 lower-case hex characters".to_owned())
        }
    }
}

impl Drop for ApiToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiToken(<redacted>)")
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        for nibble in [b >> 4, b & 0xf] {
            out.push(char::from(
                DIGITS.get(usize::from(nibble)).copied().unwrap_or(b'0'),
            ));
        }
    }
    out
}

/// Where the API listens and the token it wants: what a client needs to connect.
///
/// The server writes it with [`ConnectionInfo::write`] after binding; the CLI and UI read it
/// with [`ConnectionInfo::read`]. The file is owner-only (`puddle_fs::private` owns this): mode
/// `0600` in a `0700` directory on Unix, and on Windows a protected ACL with one entry, for the
/// current user. A file that others can read is refused when read back.
#[derive(Clone)]
pub struct ConnectionInfo {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    /// The bearer token.
    pub token: ApiToken,
}

impl fmt::Debug for ConnectionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionInfo")
            .field("url", &self.url)
            .field("token", &self.token)
            .finish()
    }
}

/// The file's JSON shape. The token is overwritten when this drops.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFormat {
    version: u32,
    url: String,
    token: String,
}

impl Drop for FileFormat {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

/// What [`ConnectionInfo::write`] serialises: the same fields as [`FileFormat`], borrowed, so the
/// token is not copied on its way out.
#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    url: &'a str,
    token: &'a str,
}

impl ConnectionInfo {
    /// Writes the file atomically (a temporary file in the same directory, then a rename), so a
    /// reader never sees half a token. Creates the directory if needed.
    ///
    /// # Errors
    ///
    /// [`ConnectionFileError::Io`] if the directory or file can't be created or written.
    pub fn write(&self, path: &Path) -> Result<(), ConnectionFileError> {
        let io_err = |source| ConnectionFileError::Io {
            path: path.to_owned(),
            source,
        };
        // Sized for a real file, so the buffer that holds the token is not reallocated as it fills.
        let mut body = Zeroizing::new(Vec::with_capacity(
            usize::try_from(MAX_FILE_BYTES).unwrap_or(4096),
        ));
        serde_json::to_writer(
            &mut *body,
            &FileOut {
                version: FILE_VERSION,
                url: &self.url,
                token: self.token.expose(),
            },
        )
        .map_err(|err| io_err(io::Error::other(err)))?;
        puddle_fs::private::write_atomic(path, &body).map_err(io_err)?;
        Ok(())
    }

    /// Reads a file written by [`ConnectionInfo::write`].
    ///
    /// # Errors
    ///
    /// [`ConnectionFileError::Io`] if it can't be read, [`ConnectionFileError::Permissions`] if
    /// others may read it (Unix), [`ConnectionFileError::Format`] if it isn't a connection file
    /// of this version.
    pub fn read(path: &Path) -> Result<Self, ConnectionFileError> {
        let io_err = |source| ConnectionFileError::Io {
            path: path.to_owned(),
            source,
        };
        let file = fs::File::open(path).map_err(io_err)?;
        puddle_fs::private::check(&file).map_err(|err| match err {
            puddle_fs::private::CheckError::Io(source) => io_err(source),
            puddle_fs::private::CheckError::Exposed(exposed) => ConnectionFileError::Permissions {
                path: path.to_owned(),
                exposed,
            },
        })?;
        let mut text = Zeroizing::new(String::with_capacity(
            usize::try_from(MAX_FILE_BYTES + 1).unwrap_or(4097),
        ));
        file.take(MAX_FILE_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(io_err)?;
        let format_err = |reason: String| ConnectionFileError::Format {
            path: path.to_owned(),
            reason,
        };
        if text.len() > usize::try_from(MAX_FILE_BYTES).unwrap_or(usize::MAX) {
            return Err(format_err("file is too large".to_owned()));
        }
        let mut parsed: FileFormat = serde_json::from_str(&text).map_err(|err| {
            // serde_json's message can quote the input; keep only where it failed.
            format_err(format!(
                "not a connection file (line {}, column {})",
                err.line(),
                err.column()
            ))
        })?;
        if parsed.version != FILE_VERSION {
            return Err(format_err(format!(
                "version {} is not supported (this Puddle reads {FILE_VERSION})",
                parsed.version
            )));
        }
        Ok(Self {
            url: std::mem::take(&mut parsed.url),
            token: ApiToken::parse(std::mem::take(&mut parsed.token)).map_err(format_err)?,
        })
    }
}

/// Why the connection file couldn't be written or read. Never contains the token.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectionFileError {
    /// The OS random source failed.
    #[error("cannot generate a random token: {0}")]
    Random(String),
    /// A file-system call failed.
    #[error("connection file {}: {source}", path.display())]
    Io {
        /// The connection file.
        path: PathBuf,
        /// What failed.
        source: io::Error,
    },
    /// The file may be read by other users.
    #[error(
        "connection file {} is readable by other users ({exposed})",
        path.display()
    )]
    Permissions {
        /// The connection file.
        path: PathBuf,
        /// How it is open to others.
        exposed: puddle_fs::private::Exposed,
    },
    /// The file isn't a connection file this Puddle understands.
    #[error("connection file {}: {reason}", path.display())]
    Format {
        /// The connection file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
}

#[cfg(test)]
mod tests;
