// SPDX-License-Identifier: GPL-3.0-or-later
//! The login key of direct SSH: the one key the runtime's SSH server accepts and the ssh config
//! entries name.
//!
//! The key pair is made once, in the data folder, in the OpenSSH formats `ssh` reads. The host
//! hands the runtime only the public line. The private file is written owner-only and is never
//! logged or shown (the host reads it into memory only to sign in for a terminal): whether a pair exists is decided by its public half. A private
//! file that is already there is made owner-only again if it was loosened (`ssh` refuses a key
//! file others can read), and one that cannot be is replaced by a new pair.

use std::fmt::{self, Write as _};
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use zeroize::Zeroizing;

/// The folder below the data folder that holds the login key and the ssh config files.
pub(crate) const SSH_DIR: &str = "ssh";
const PRIVATE_FILE: &str = "login_ed25519";
const PUBLIC_FILE: &str = "login_ed25519.pub";
const ALGORITHM: &[u8] = b"ssh-ed25519";
const COMMENT: &str = "puddle";
/// The armor label of an OpenSSH private key file (kept apart from the dashes, so that a scan for
/// committed keys has nothing to match in the source).
const PEM_KIND: &str = "OPENSSH PRIVATE KEY";

/// Puddle's login key: where the private half is and what the public half says.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LoginKey {
    private: PathBuf,
    public_line: String,
}

impl fmt::Debug for LoginKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginKey")
            .field("private", &self.private)
            .finish_non_exhaustive()
    }
}

impl LoginKey {
    /// The key pair in `dir`, made when either half is missing.
    ///
    /// # Errors
    ///
    /// Any file-system error, or the OS random source failing.
    pub(crate) fn open(dir: &Path) -> io::Result<Self> {
        let private = dir.join(PRIVATE_FILE);
        let public = dir.join(PUBLIC_FILE);
        if private.is_file()
            && let Ok(line) = std::fs::read_to_string(&public)
            && let Some(line) = public_line_of(&line)
        {
            match puddle_fs::private::tighten_file(&private) {
                Ok(was) => {
                    if let Some(was) = was {
                        tracing::warn!(file = %private.display(), %was, "the login key's file was open to others; it is owner-only again");
                    }
                    return Ok(Self {
                        private,
                        public_line: line,
                    });
                }
                Err(e) => {
                    tracing::warn!(file = %private.display(), error = %e, "the login key's file cannot be made owner-only; making a new pair");
                }
            }
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *seed).map_err(io::Error::other)?;
        let key = SigningKey::from_bytes(&seed);
        let public_bytes = key.verifying_key().to_bytes();
        let public_line = format!(
            "ssh-ed25519 {} {COMMENT}",
            STANDARD.encode(public_blob(&public_bytes))
        );
        // The private half first: a public file without it is what `open` ignores.
        puddle_fs::private::write_atomic(&private, private_file(&seed, &public_bytes).as_bytes())?;
        puddle_fs::private::write_atomic(&public, format!("{public_line}\n").as_bytes())?;
        Ok(Self {
            private,
            public_line,
        })
    }

    /// The private key's file, for the ssh config's `IdentityFile`.
    pub(crate) fn private_path(&self) -> &Path {
        &self.private
    }

    /// The public key as an OpenSSH line, the form the runtime's SSH server takes.
    pub(crate) fn public_line(&self) -> &str {
        &self.public_line
    }
}

/// The one `ssh-ed25519 <base64> [comment]` line of a `.pub` file, if the file holds just that.
fn public_line_of(text: &str) -> Option<String> {
    let line = text.trim();
    let mut fields = line.split_whitespace();
    let valid = fields.next() == Some("ssh-ed25519")
        && fields
            .next()
            .and_then(|blob| STANDARD.decode(blob).ok())
            .is_some_and(|blob| blob.starts_with(&ssh_string(ALGORITHM)))
        && !line.contains(['\n', '\r']);
    valid.then(|| line.to_owned())
}

fn ssh_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

fn public_blob(public: &[u8; 32]) -> Vec<u8> {
    let mut blob = ssh_string(ALGORITHM);
    blob.extend(ssh_string(public));
    blob
}

/// Room for the private section of an Ed25519 key (about 150 bytes, plus padding).
const SECTION_CAPACITY: usize = 256;
/// Room for the whole binary key file (about 260 bytes).
const BODY_CAPACITY: usize = 512;
/// Room for the PEM text of the key (about 400 characters).
const TEXT_CAPACITY: usize = 768;

/// The unencrypted `openssh-key-v1` file for the key `seed`/`public`. Every buffer that holds
/// the secret is wiped when it goes.
fn private_file(seed: &[u8; 32], public: &[u8; 32]) -> Zeroizing<String> {
    let mut check = [0u8; 4];
    // The check words only detect a wrong passphrase, which an unencrypted key has none of.
    check.copy_from_slice(&public[..4]);
    // Every buffer below is sized for the largest key this writes, so none grows and frees a
    // smaller, unwiped copy of the seed.
    let mut section = Zeroizing::new(Vec::with_capacity(SECTION_CAPACITY));
    section.extend_from_slice(&check);
    section.extend_from_slice(&check);
    section.extend(ssh_string(ALGORITHM));
    section.extend(ssh_string(public));
    let mut secret = Zeroizing::new(Vec::with_capacity(seed.len() + public.len()));
    secret.extend_from_slice(seed);
    secret.extend_from_slice(public);
    // Appended in place: `ssh_string` would leave an unwiped copy of the secret behind.
    section.extend_from_slice(
        &u32::try_from(secret.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    section.extend_from_slice(&secret);
    section.extend(ssh_string(COMMENT.as_bytes()));
    let mut pad = 1u8;
    while section.len() % 8 != 0 {
        section.push(pad);
        pad += 1;
    }
    let mut body = Zeroizing::new(Vec::with_capacity(BODY_CAPACITY));
    body.extend_from_slice(b"openssh-key-v1\0");
    body.extend(ssh_string(b"none"));
    body.extend(ssh_string(b"none"));
    body.extend(ssh_string(b""));
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend(ssh_string(&public_blob(public)));
    body.extend_from_slice(&Zeroizing::new(ssh_string(&section)));
    let encoded = Zeroizing::new(STANDARD.encode(&*body));
    let mut text = Zeroizing::new(String::with_capacity(TEXT_CAPACITY));
    let _ = writeln!(text, "-----BEGIN {PEM_KIND}-----");
    for chunk in encoded.as_bytes().chunks(70) {
        text.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        text.push('\n');
    }
    let _ = writeln!(text, "-----END {PEM_KIND}-----");
    text
}

#[cfg(test)]
mod tests;
