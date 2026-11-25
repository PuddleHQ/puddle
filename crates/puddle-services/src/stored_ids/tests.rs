// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn id(text: &str) -> StoredId {
    StoredId::new(text).unwrap()
}

#[test]
fn the_owners_names_are_unchanged_and_another_users_carry_their_id() {
    let ann = UserId::from_stored(7);
    assert_eq!(name(UserId::OWNER, "tok-1"), "tok-1");
    assert_eq!(name(ann, "tok-1"), "u7-tok-1");
    let owners = fresh(UserId::OWNER, "env").unwrap();
    let anns = fresh(ann, "env").unwrap();
    assert!(owners.as_str().starts_with("env-") && owners.as_str().len() == 36);
    assert!(anns.as_str().starts_with("u7-env-") && anns.as_str().len() == 39);
    assert_ne!(fresh(ann, "env").unwrap(), anns);
    assert!(is_users(UserId::OWNER, &owners) && !is_users(ann, &owners));
    assert!(is_users(ann, &anns) && !is_users(UserId::OWNER, &anns));
}

#[test]
fn a_name_that_only_looks_like_a_users_is_the_owners() {
    for text in [
        "u-tok",
        "ux-tok",
        "u7tok",
        "unknown",
        "u-7-tok",
        "u99999999999999999999-x",
    ] {
        assert!(is_users(UserId::OWNER, &id(text)), "{text}");
    }
    assert!(is_users(UserId::from_stored(12), &id("u12-tok-a")));
    assert!(!is_users(UserId::from_stored(1), &id("u12-tok-a")));
}
