// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_secrets::{AccountName, HostName, TokenScope};

use super::*;

fn id(text: &str) -> StoredId {
    StoredId::new(text).unwrap()
}

fn stored(text: &str) -> SourceSpec {
    SourceSpec::Stored {
        id: id(text),
        scope: TokenScope {
            host: HostName::new("github.com").unwrap(),
            org: None,
        },
    }
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

#[test]
fn a_source_is_usable_only_by_its_user() {
    let ann = UserId::from_stored(7);
    let gh = SourceSpec::Gh {
        host: HostName::new("github.com").unwrap(),
        account: AccountName::new("me").unwrap(),
    };
    assert!(check_source(UserId::OWNER, &gh).is_ok());
    assert!(check_source(UserId::OWNER, &stored("tok-a")).is_ok());
    assert!(check_source(ann, &stored("u7-tok-a")).is_ok());
    // Another user's entry is refused the same way whether it exists or not.
    let theirs = check_source(ann, &stored("tok-a")).unwrap_err();
    let made_up = check_source(ann, &stored("u8-tok-a")).unwrap_err();
    assert_eq!(theirs.status(), made_up.status());
    assert_eq!(format!("{theirs:?}"), format!("{made_up:?}"));
    assert!(check_source(UserId::OWNER, &stored("u7-tok-a")).is_err());
    // The computer's signed-in accounts are the owner's.
    assert_eq!(
        check_source(ann, &gh).unwrap_err().status(),
        axum::http::StatusCode::FORBIDDEN
    );
}
