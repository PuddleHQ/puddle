// SPDX-License-Identifier: AGPL-3.0-or-later
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
