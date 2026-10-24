// SPDX-License-Identifier: GPL-3.0-or-later
//! The login key of direct SSH: the one key the runtime's SSH server accepts and the ssh config
//! entries name.
//!
//! The key pair is made once, in the data folder, in the OpenSSH formats `ssh` reads. The host
//! hands the runtime only the public line. The private file is written owner-only and is never
//! read back, logged or shown: whether a pair exists is decided by its public half. A private
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
        getrandom::fill(&mut *seed).map_err(|e| io::Error::other(e.to_string()))?;
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
    let mut out = u32::try_from(bytes.len())
        .unwrap_or(u32::MAX)
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(bytes);
    out
}

fn public_blob(public: &[u8; 32]) -> Vec<u8> {
    let mut blob = ssh_string(ALGORITHM);
    blob.extend(ssh_string(public));
    blob
}

/// The unencrypted `openssh-key-v1` file for the key `seed`/`public`. Every buffer that holds
/// the secret is wiped when it goes.
fn private_file(seed: &[u8; 32], public: &[u8; 32]) -> Zeroizing<String> {
    let mut check = [0u8; 4];
    // The check words only detect a wrong passphrase, which an unencrypted key has none of.
    check.copy_from_slice(&public[..4]);
    let mut section = Zeroizing::new(Vec::new());
    section.extend_from_slice(&check);
    section.extend_from_slice(&check);
    section.extend(ssh_string(ALGORITHM));
    section.extend(ssh_string(public));
    let mut secret = Zeroizing::new(seed.to_vec());
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
    let mut body = Zeroizing::new(b"openssh-key-v1\0".to_vec());
    body.extend(ssh_string(b"none"));
    body.extend(ssh_string(b"none"));
    body.extend(ssh_string(b""));
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend(ssh_string(&public_blob(public)));
    body.extend_from_slice(&Zeroizing::new(ssh_string(&section)));
    let encoded = Zeroizing::new(STANDARD.encode(&*body));
    let mut text = Zeroizing::new(format!("-----BEGIN {PEM_KIND}-----\n"));
    for chunk in encoded.as_bytes().chunks(70) {
        text.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        text.push('\n');
    }
    let _ = writeln!(text, "-----END {PEM_KIND}-----");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_pair_is_made_once_and_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let first = LoginKey::open(dir.path()).unwrap();
        assert!(first.public_line().starts_with("ssh-ed25519 AAAA"));
        assert!(first.public_line().ends_with(&format!(" {COMMENT}")));
        assert!(first.private_path().is_file());
        let second = LoginKey::open(dir.path()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_missing_private_half_or_a_damaged_public_half_makes_a_new_pair() {
        let dir = tempfile::tempdir().unwrap();
        let first = LoginKey::open(dir.path()).unwrap();
        std::fs::remove_file(first.private_path()).unwrap();
        let second = LoginKey::open(dir.path()).unwrap();
        assert_ne!(first.public_line(), second.public_line());
        std::fs::write(dir.path().join(PUBLIC_FILE), "not a key\n").unwrap();
        let third = LoginKey::open(dir.path()).unwrap();
        assert_ne!(second.public_line(), third.public_line());
        assert!(third.private_path().is_file());
    }

    #[test]
    fn debug_never_shows_key_material() {
        let dir = tempfile::tempdir().unwrap();
        let key = LoginKey::open(dir.path()).unwrap();
        let shown = format!("{key:?}");
        let blob = key.public_line().split(' ').nth(1).unwrap();
        assert!(!shown.contains(blob), "{shown}");
    }

    #[cfg(unix)]
    #[test]
    fn the_private_file_is_owner_only_and_ssh_derives_the_public_line_from_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let key = LoginKey::open(dir.path()).unwrap();
        let mode = std::fs::metadata(key.private_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
        let out = std::process::Command::new("ssh-keygen")
            .args(["-y", "-f"])
            .arg(key.private_path())
            .output()
            .expect("ssh-keygen runs (OpenSSH client is a test requirement)");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let derived = String::from_utf8(out.stdout).unwrap();
        let mine: Vec<_> = key.public_line().split(' ').take(2).collect();
        let theirs: Vec<_> = derived.trim().split(' ').take(2).collect();
        assert_eq!(mine, theirs);
    }

    #[cfg(unix)]
    #[test]
    fn a_loosened_private_file_is_made_owner_only_and_the_same_key_is_kept() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let first = LoginKey::open(dir.path()).unwrap();
        std::fs::set_permissions(first.private_path(), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let again = LoginKey::open(dir.path()).unwrap();
        assert_eq!(first, again);
        let mode = std::fs::metadata(again.private_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn a_private_path_that_is_a_link_is_replaced_by_a_new_pair_and_the_link_target_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let first = LoginKey::open(dir.path()).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        let original = std::fs::read(first.private_path()).unwrap();
        std::fs::rename(first.private_path(), &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, first.private_path()).unwrap();
        let second = LoginKey::open(dir.path()).unwrap();
        assert_ne!(first.public_line(), second.public_line());
        let meta = std::fs::symlink_metadata(second.private_path()).unwrap();
        assert!(meta.is_file() && !meta.is_symlink());
        assert_eq!(std::fs::read(&elsewhere).unwrap(), original);
    }
}
