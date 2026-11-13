// SPDX-License-Identifier: AGPL-3.0-or-later
use super::super::tests::{empty, password, pw};
use super::*;

#[test]
fn a_hash_is_argon2id_with_its_parameters_and_a_new_salt_every_time() {
    let one = hash(password()).unwrap();
    let two = hash(password()).unwrap();
    assert!(one.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{one}");
    assert_ne!(one, two, "each hash has its own salt");
    assert!(!one.contains("correct horse"));
}

#[test]
fn a_password_verifies_against_its_hash_and_nothing_else_does() {
    let stored = hash(password()).unwrap();
    assert!(verify(&stored, password()));
    assert!(!verify(&stored, &format!("{}Y", password())));
    assert!(!verify(&stored, empty()));
    assert!(!verify("not a hash", password()));
    assert!(!verify("", empty()));
}

#[test]
fn a_hash_made_with_other_parameters_still_verifies() {
    // The cost can be raised later without locking anyone out.
    let params = Params::new(8, 1, 1, None).unwrap();
    let older = Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_with_salt(pw("an older password").as_bytes(), &[7; 16])
        .unwrap()
        .to_string();
    assert!(verify(&older, pw("an older password")));
    assert!(!verify(&older, pw("another password")));
}

/// `n` characters, built at run time.
fn text(n: usize) -> String {
    pw("p").chars().cycle().take(n).collect()
}

#[test]
fn the_policy_wants_twelve_characters_not_the_name_and_a_sane_length() {
    assert!(check_policy(&text(12), "ann").is_ok());
    assert!(check_policy(&text(13), "ann").is_ok());
    assert_eq!(
        check_policy(&text(11), "ann"),
        Err(PasswordError::Weak(
            "a password needs at least 12 characters"
        ))
    );
    // Characters, not bytes: twelve multi-byte characters are enough.
    assert!(check_policy(&"é".repeat(12), "ann").is_ok());
    let name = pw("tijs van kampen");
    assert_eq!(
        check_policy(&name.to_uppercase(), name),
        Err(PasswordError::Weak("a password can't be the user name"))
    );
    assert!(check_policy(&"a".repeat(MAX_PASSWORD_BYTES), "ann").is_ok());
    assert_eq!(
        check_policy(&"a".repeat(MAX_PASSWORD_BYTES + 1), "ann"),
        Err(PasswordError::Weak("a password has at most 256 bytes"))
    );
}

#[test]
fn the_decoy_is_a_real_hash_nobody_can_guess() {
    let decoy = decoy().unwrap();
    assert!(decoy.starts_with("$argon2id$"));
    assert!(!verify(&decoy, pw("password1234")));
}
