// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host keys one start of a workspace's sandbox serves for Git over SSH.
//!
//! Each start makes one Ed25519, one ECDSA P-256 and one RSA 3072 key from the operating
//! system's random numbers. They live in memory only: nothing here writes them anywhere, and
//! they are dropped (and zeroed by the key type) when the start ends, like the workspace's CA.
//! Their public halves go to the guest's `ssh_known_hosts` through [`GitSshHostKeys::known_hosts`].

use std::fmt;

use puddle_types::Host;
use russh::keys::ssh_key::private::{KeypairData, RsaKeypair};
use russh::keys::ssh_key::{Algorithm, EcdsaCurve, rand_core};

use super::GitSshHostKey;

/// The RSA modulus size; 3072 bits is what OpenSSH's own `ssh-keygen -t rsa -b 3072` guides use
/// and the size the NIST SP 800-57 advice gives for 128-bit security.
const RSA_BITS: usize = 3072;

/// The port the guest's `ssh` writes in brackets for a host on 443 (`ssh.github.com`).
const ALTERNATE_PORT: &str = "443";

/// Why a start could not make its host keys.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot make the SSH host keys: {0}")]
pub struct GitSshKeyError(String);

/// Random numbers from `fill`. A failure cannot be reported through the key crate's
/// infallible interface and a stream of zeros would panic its scalar check or never end its
/// prime search, so a failure is latched and the bytes come from a throwaway generator that only
/// has to keep the key code finishing: [`GitSshHostKeys::generate_with`] then drops every key.
struct Source {
    fill: fn(&mut [u8]) -> Result<(), getrandom::Error>,
    failed: bool,
    /// `SplitMix64` state of the throwaway generator.
    state: u64,
}

impl Source {
    fn new(fill: fn(&mut [u8]) -> Result<(), getrandom::Error>) -> Self {
        Self {
            fill,
            failed: false,
            state: 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn bytes(&mut self, dst: &mut [u8]) {
        if !self.failed && (self.fill)(dst).is_ok() {
            return;
        }
        self.failed = true;
        for chunk in dst.chunks_mut(8) {
            self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            for (dst, src) in chunk.iter_mut().zip(z.to_le_bytes()) {
                *dst = src;
            }
        }
    }
}

impl rand_core::TryRng for Source {
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0; 4];
        self.bytes(&mut bytes);
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0; 8];
        self.bytes(&mut bytes);
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.bytes(dst);
        Ok(())
    }
}

impl rand_core::TryCryptoRng for Source {}

/// One start's host keys: Ed25519, ECDSA P-256 and RSA 3072, in the order a client prefers them.
#[derive(Clone)]
pub struct GitSshHostKeys {
    keys: Vec<GitSshHostKey>,
    /// `keytype base64` of each key, in the same order.
    public: Vec<String>,
}

impl fmt::Debug for GitSshHostKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A key never reaches a log, not even through a derived `Debug`.
        f.debug_struct("GitSshHostKeys")
            .field("keys", &self.keys.len())
            .finish_non_exhaustive()
    }
}

impl GitSshHostKeys {
    /// Makes the three keys. The RSA key takes a noticeable moment (about a second in a release
    /// build), so call it off the async threads.
    ///
    /// # Errors
    ///
    /// When the operating system's random numbers fail or a key cannot be made.
    pub fn generate() -> Result<Self, GitSshKeyError> {
        Self::generate_with(getrandom::fill)
    }

    /// [`Self::generate`] with another source of random bytes (a test's failing one).
    fn generate_with(
        fill: fn(&mut [u8]) -> Result<(), getrandom::Error>,
    ) -> Result<Self, GitSshKeyError> {
        let fail = |e: &dyn fmt::Display| GitSshKeyError(e.to_string());
        let mut rng = Source::new(fill);
        let ed25519 = GitSshHostKey::random(&mut rng, Algorithm::Ed25519).map_err(|e| fail(&e))?;
        let curve = EcdsaCurve::NistP256;
        let ecdsa =
            GitSshHostKey::random(&mut rng, Algorithm::Ecdsa { curve }).map_err(|e| fail(&e))?;
        let rsa = RsaKeypair::random(&mut rng, RSA_BITS).map_err(|e| fail(&e))?;
        let rsa = GitSshHostKey::new(KeypairData::from(rsa), "").map_err(|e| fail(&e))?;
        if rng.failed {
            return Err(fail(&"the operating system's random numbers failed"));
        }
        let keys = vec![ed25519, ecdsa, rsa];
        let public = keys
            .iter()
            .map(|key| public_half(key).ok_or_else(|| fail(&"a public key has no text form")))
            .collect::<Result<_, _>>()?;
        Ok(Self { keys, public })
    }

    /// The private keys, for [`super::GitSsh::new`].
    #[must_use]
    pub fn keys(&self) -> &[GitSshHostKey] {
        &self.keys
    }

    /// The keys, for [`super::GitSsh::new`].
    #[must_use]
    pub fn into_keys(self) -> Vec<GitSshHostKey> {
        self.keys
    }

    /// The `known_hosts` lines that list these keys under every name in `hosts`, once as the
    /// name and once as `[name]:443` (a client that reaches the host on 443 writes the port),
    /// one line per name and key, without a final line break. Addresses are left out: only names
    /// are terminated. `None` when no name is left.
    #[must_use]
    pub fn known_hosts(&self, hosts: &[Host]) -> Option<String> {
        let mut names: Vec<String> = hosts
            .iter()
            .filter_map(Host::name)
            .map(ToString::to_string)
            .collect();
        names.sort();
        names.dedup();
        let keys = &self.public;
        let lines: Vec<String> = names
            .iter()
            .flat_map(|name| {
                let patterns = [name.clone(), format!("[{name}]:{ALTERNATE_PORT}")];
                patterns.into_iter().flat_map(move |pattern| {
                    keys.iter()
                        .map(move |key| format!("{pattern} {key}"))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }
}

/// `keytype base64`, the public half as `known_hosts` writes it (no comment).
fn public_half(key: &GitSshHostKey) -> Option<String> {
    let line = key.public_key().to_openssh().ok()?;
    let mut words = line.split_whitespace();
    Some(format!("{} {}", words.next()?, words.next()?))
}

#[cfg(test)]
mod tests;
