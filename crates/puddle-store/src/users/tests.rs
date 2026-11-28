// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_user_name_is_one_to_64_visible_characters_with_no_outer_space() {
    assert_eq!(UserName::new("Ann").unwrap().as_str(), "Ann");
    assert!(UserName::new(&"é".repeat(64)).is_ok());
    for bad in ["", " ann", "ann ", "a\nb", "a\u{7f}b"] {
        assert!(UserName::new(bad).is_err(), "{bad:?}");
    }
    let long = "a".repeat(65);
    assert_eq!(
        UserName::new(&long).unwrap_err().to_string(),
        "a user name has at most 64 characters"
    );
}

#[test]
fn names_refuse_the_characters_that_do_not_show() {
    // Zero-width space, joiner, left-to-right mark, bidi override and isolate, word joiner,
    // byte-order mark, soft hyphen, tag character.
    for hidden in [
        '\u{200B}',
        '\u{200D}',
        '\u{200E}',
        '\u{202E}',
        '\u{2066}',
        '\u{2060}',
        '\u{FEFF}',
        '\u{00AD}',
        '\u{E0041}',
    ] {
        let name = format!("an{hidden}n");
        assert!(UserName::new(&name).is_err(), "{name:?}");
        assert!(device_name(&name).is_err(), "{name:?}");
    }
    assert!(UserName::new("Zoë 名前").is_ok());
    assert_eq!(device_name(" Ann's laptop ").unwrap(), "Ann's laptop");
}

#[test]
fn a_role_reads_back_as_written() {
    for role in [Role::Owner, Role::User] {
        assert_eq!(Role::parse(role.as_str()), Some(role));
    }
    assert_eq!(Role::parse("admin"), None);
}

#[test]
fn the_owner_is_user_one_and_ids_show_as_numbers() {
    assert_eq!(UserId::OWNER.get(), 1);
    assert_eq!(UserId::from_stored(7).to_string(), "7");
    assert_eq!(DeviceId::from_stored(3).get(), 3);
}

#[test]
fn a_sign_in_method_reads_back_as_written_and_an_unknown_one_does_not() {
    for method in [SignInMethod::Password, SignInMethod::Passkey] {
        assert_eq!(SignInMethod::parse(method.as_str()), Some(method));
    }
    assert_eq!(SignInMethod::parse("magic link"), None);
}
