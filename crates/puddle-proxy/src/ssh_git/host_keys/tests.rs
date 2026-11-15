// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let algorithms: Vec<Algorithm> = made().keys().iter().map(GitSshHostKey::algorithm).collect();
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
