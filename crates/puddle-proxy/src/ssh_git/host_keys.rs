// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use std::sync::OnceLock;

    use russh::keys::PublicKey;

    use super::*;

    /// One set for the whole test binary: the RSA key takes seconds in a debug build.
    fn made() -> &'static GitSshHostKeys {
        static KEYS: OnceLock<GitSshHostKeys> = OnceLock::new();
        KEYS.get_or_init(|| GitSshHostKeys::generate().unwrap())
    }

    fn host(name: &str) -> Host {
        Host::parse_normalised(name).unwrap()
    }

    #[test]
    fn a_start_makes_an_ed25519_an_ecdsa_p256_and_an_rsa_3072_key() {
        let algorithms: Vec<Algorithm> =
            made().keys().iter().map(GitSshHostKey::algorithm).collect();
        assert_eq!(
            algorithms,
            [
                Algorithm::Ed25519,
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP256
                },
                Algorithm::Rsa { hash: None },
            ]
        );
        let rsa = made().keys()[2].public_key().key_data().rsa().unwrap();
        assert_eq!(rsa.key_size(), 3072);
        // `into_keys` hands over the same keys.
        assert_eq!(made().clone().into_keys().len(), 3);
    }

    #[test]
    fn two_starts_make_different_keys() {
        let other = GitSshHostKeys::generate().unwrap();
        for (a, b) in made().keys().iter().zip(other.keys()) {
            assert_ne!(a.public_key(), b.public_key());
        }
    }

    #[test]
    fn the_known_hosts_lines_name_every_host_with_every_key_and_both_forms() {
        let keys = made();
        let hosts = [
            host("ssh.github.com"),
            host("github.com"),
            host("github.com"),
        ];
        let text = keys.known_hosts(&hosts).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // 2 names, 2 spellings, 3 keys; sorted by name, the plain name first.
        assert_eq!(lines.len(), 12, "{text}");
        let halves: Vec<String> = keys
            .keys()
            .iter()
            .map(|k| public_half(k).unwrap())
            .collect();
        let mut expected = Vec::new();
        for name in ["github.com", "ssh.github.com"] {
            for pattern in [name.to_owned(), format!("[{name}]:443")] {
                for half in &halves {
                    expected.push(format!("{pattern} {half}"));
                }
            }
        }
        assert_eq!(lines, expected);
        assert!(!text.ends_with('\n'));
        // Every line is a public key OpenSSH reads, and nothing but the three words.
        for line in lines {
            let mut words = line.split(' ');
            let (_, kind, blob) = (words.next(), words.next().unwrap(), words.next().unwrap());
            assert_eq!(words.next(), None);
            PublicKey::from_openssh(&format!("{kind} {blob}")).unwrap();
        }
    }

    #[test]
    fn an_address_has_no_known_hosts_line_and_no_name_means_no_block() {
        let keys = made();
        assert_eq!(keys.known_hosts(&[]), None);
        let address = host("192.0.2.7");
        assert_eq!(keys.known_hosts(std::slice::from_ref(&address)), None);
        let text = keys.known_hosts(&[address, host("gitlab.com")]).unwrap();
        assert!(!text.contains("192.0.2.7"), "{text}");
        assert!(text.starts_with("gitlab.com "));
    }

    #[test]
    fn a_failed_source_hands_out_the_splitmix64_stream_in_every_width() {
        use rand_core::TryRng;
        let mut source = Source::new(|_| Err(getrandom::Error::UNSUPPORTED));
        assert_eq!(source.try_next_u64().unwrap(), 0x6e78_9e6a_a1b9_65f4);
        assert!(source.failed);
        // The low half of the second output, then seven bytes of the third.
        assert_eq!(source.try_next_u32().unwrap(), 0x8009_454f);
        let mut seven = [0; 7];
        source.try_fill_bytes(&mut seven).unwrap();
        assert_eq!(seven, [0xec, 0x81, 0x4c, 0x72, 0xa8, 0xb8, 0x8b]);
    }

    #[test]
    fn random_numbers_that_fail_make_no_keys() {
        let failing = |_: &mut [u8]| Err(getrandom::Error::UNSUPPORTED);
        let err = GitSshHostKeys::generate_with(failing).unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot make the SSH host keys: the operating system's random numbers failed"
        );
    }

    #[test]
    fn debug_shows_a_count_and_never_a_key() {
        let shown = format!("{:?}", made());
        assert_eq!(shown, "GitSshHostKeys { keys: 3, .. }");
    }
}
