// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn a_hash_is_argon2id_with_its_parameters_and_a_new_salt_every_time() {
    let one = hash("correct horse battery").unwrap();
    let two = hash("correct horse battery").unwrap();
    assert!(one.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{one}");
    assert_ne!(one, two, "each hash has its own salt");
    assert!(!one.contains("correct horse"));
}

#[test]
fn a_password_verifies_against_its_hash_and_nothing_else_does() {
    let stored = hash("correct horse battery").unwrap();
    assert!(verify(&stored, "correct horse battery"));
    assert!(!verify(&stored, "correct horse batterY"));
    assert!(!verify(&stored, ""));
    assert!(!verify("not a hash", "correct horse battery"));
    assert!(!verify("", ""));
}

#[test]
fn a_hash_made_with_other_parameters_still_verifies() {
    // The cost can be raised later without locking anyone out.
    let params = Params::new(8, 1, 1, None).unwrap();
    let older = Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_with_salt(b"an older password", &[7; 16])
        .unwrap()
        .to_string();
    assert!(verify(&older, "an older password"));
    assert!(!verify(&older, "another password"));
}

#[test]
fn the_policy_wants_twelve_characters_not_the_name_and_a_sane_length() {
    assert!(check_policy("twelve chars", "ann").is_ok());
    assert!(check_policy("elf chars!!!", "ann").is_ok());
    assert_eq!(
        check_policy("eleven char", "ann"),
        Err(PasswordError::Weak(
            "a password needs at least 12 characters"
        ))
    );
    // Characters, not bytes: twelve multi-byte characters are enough.
    assert!(check_policy(&"é".repeat(12), "ann").is_ok());
    assert_eq!(
        check_policy("Tijs van Kampen", "tijs van kampen"),
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
    assert!(!verify(&decoy, "password1234"));
}
