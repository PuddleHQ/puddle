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
