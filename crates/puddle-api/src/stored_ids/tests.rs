// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_secrets::{AccountName, HostName, StoredId, TokenScope};

use super::*;

fn stored(text: &str) -> SourceSpec {
    SourceSpec::Stored {
        id: StoredId::new(text).unwrap(),
        scope: TokenScope {
            host: HostName::new("github.com").unwrap(),
            org: None,
        },
    }
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
