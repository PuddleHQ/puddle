// SPDX-License-Identifier: GPL-3.0-or-later
//! The keys an agent holds.

use std::fmt;

use crate::wire;

/// One key an agent holds. The comment is whatever the user or tool wrote when adding it (often an
/// email address or a path), so it is never shown outside the user's own machine and `Debug` leaves
/// it out.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    blob: Vec<u8>,
    comment: String,
}

impl Identity {
    /// A key from its public blob (wire format) and comment.
    #[must_use]
    pub fn new(blob: Vec<u8>, comment: String) -> Self {
        Self { blob, comment }
    }

    /// The public key in SSH wire format (the blob a signature request names).
    #[must_use]
    pub fn blob(&self) -> &[u8] {
        &self.blob
    }

    /// The comment the agent keeps with the key.
    #[must_use]
    pub fn comment(&self) -> &str {
        &self.comment
    }

    /// The key's algorithm name from the start of its blob (`ssh-ed25519`, `ssh-rsa`,
    /// `ecdsa-sha2-nistp256`), or `None` when the blob does not start with one.
    #[must_use]
    pub fn key_type(&self) -> Option<&str> {
        let mut rest = self.blob.as_slice();
        let name = wire::take_string(&mut rest).ok()?;
        std::str::from_utf8(name).ok().filter(|n| !n.is_empty())
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("key_type", &self.key_type())
            .field("blob_len", &self.blob.len())
            .finish_non_exhaustive()
    }
}

/// A key algorithm name as a short family for people: `ssh-ed25519` is `ed25519`,
/// `ecdsa-sha2-nistp256` is `ecdsa`, `ssh-rsa` is `rsa`, `ssh-dss` is `dsa`, a hardware key
/// (`sk-ssh-ed25519@openssh.com`) is `sk-ed25519`, and a certificate counts as the key it
/// certifies. Anything else is `other`, so a report never repeats a name an agent made up.
#[must_use]
pub fn key_family(key_type: &str) -> &'static str {
    let base = key_type
        .strip_suffix("-cert-v01@openssh.com")
        .unwrap_or(key_type);
    match base {
        "ssh-ed25519" => "ed25519",
        "ssh-rsa" => "rsa",
        "ssh-dss" => "dsa",
        "sk-ssh-ed25519@openssh.com" => "sk-ed25519",
        "sk-ecdsa-sha2-nistp256@openssh.com" => "sk-ecdsa",
        "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" | "ecdsa-sha2-nistp521" => "ecdsa",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(name: &str) -> Vec<u8> {
        let mut v = u32::try_from(name.len()).unwrap().to_be_bytes().to_vec();
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(&[1, 2, 3]);
        v
    }

    #[test]
    fn the_key_type_is_the_first_string_of_the_blob() {
        let id = Identity::new(blob("ssh-ed25519"), "me@example".into());
        assert_eq!(id.key_type(), Some("ssh-ed25519"));
        assert_eq!(id.comment(), "me@example");
        assert_eq!(id.blob(), blob("ssh-ed25519").as_slice());
    }

    #[test]
    fn a_blob_that_does_not_start_with_a_name_has_no_type() {
        for bad in [vec![], vec![0, 0, 0], vec![0, 0, 0, 9, b'a'], blob("")] {
            assert_eq!(Identity::new(bad, String::new()).key_type(), None);
        }
        let not_utf8 = vec![0, 0, 0, 1, 0xFF];
        assert_eq!(Identity::new(not_utf8, String::new()).key_type(), None);
    }

    #[test]
    fn debug_never_shows_the_comment_or_the_key() {
        let id = Identity::new(blob("ssh-rsa"), "secret-comment".into());
        let text = format!("{id:?}");
        assert!(text.contains("ssh-rsa"), "{text}");
        assert!(!text.contains("secret-comment"), "{text}");
    }

    #[test]
    fn families() {
        let cases = [
            ("ssh-ed25519", "ed25519"),
            ("ssh-rsa", "rsa"),
            ("ssh-dss", "dsa"),
            ("ecdsa-sha2-nistp256", "ecdsa"),
            ("ecdsa-sha2-nistp384", "ecdsa"),
            ("ecdsa-sha2-nistp521", "ecdsa"),
            ("sk-ssh-ed25519@openssh.com", "sk-ed25519"),
            ("sk-ecdsa-sha2-nistp256@openssh.com", "sk-ecdsa"),
            ("ssh-ed25519-cert-v01@openssh.com", "ed25519"),
            ("ssh-rsa-cert-v01@openssh.com", "rsa"),
            ("whatever@example.com", "other"),
            ("", "other"),
        ];
        for (name, family) in cases {
            assert_eq!(key_family(name), family, "{name}");
        }
    }
}
