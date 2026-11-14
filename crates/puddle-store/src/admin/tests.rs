// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn every_action_reads_back_from_its_stored_spelling_and_an_unknown_one_does_not() {
    for action in [
        AdminAction::UserCreated,
        AdminAction::InviteReissued,
        AdminAction::UserDeleteStarted,
        AdminAction::UserDeleted,
    ] {
        assert_eq!(AdminAction::parse(action.as_str()), Some(action));
    }
    assert_eq!(AdminAction::parse("user_impersonated"), None);
}
