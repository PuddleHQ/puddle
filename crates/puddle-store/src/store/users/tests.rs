// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;
use crate::audit::{WorkspaceOpKind, WorkspaceOpOutcome};
use crate::clock::ManualClock;
use crate::rule::Actor;
use crate::store::{AuditCursor, AuditFilter, Limits, NewWorkspaceOp};

fn store() -> Store {
    Store::open_in_memory(Arc::new(ManualClock::new(5)), Limits::default()).unwrap()
}

fn ws(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

fn name(text: &str) -> UserName {
    UserName::new(text).unwrap()
}

#[test]
fn a_new_database_has_the_owner_and_nobody_else() {
    let users = store().users().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].id, UserId::OWNER);
    assert_eq!(users[0].role, Role::Owner);
    assert_eq!(users[0].name, "owner");
}

#[test]
fn added_users_are_plain_users_with_unique_names() {
    let store = store();
    let ann = store.add_user(&name("Ann")).unwrap();
    assert_eq!(
        (ann.role, ann.name.as_str(), ann.created_at),
        (Role::User, "Ann", 5)
    );
    assert_ne!(ann.id, UserId::OWNER);
    assert_eq!(store.user(ann.id).unwrap(), ann);
    let err = store.add_user(&name("ANN")).unwrap_err();
    assert!(matches!(err, StoreError::UserName(_)));
    let ids: Vec<UserId> = store.users().unwrap().iter().map(|u| u.id).collect();
    assert_eq!(ids, [UserId::OWNER, ann.id]);
    assert!(matches!(
        store.user(UserId::from_stored(99)),
        Err(StoreError::UnknownUser(_))
    ));
}

#[test]
fn a_workspace_with_no_recorded_user_is_nobodys() {
    let store = store();
    assert_eq!(store.workspace_owner(&ws("box")).unwrap(), None);
    let owners = store.workspace_owners().unwrap();
    assert!(!owners.is(&ws("box"), UserId::OWNER));
}

/// The first start of this build records the owner for the workspaces it lists; after that a
/// workspace with no row stays nobody's, whoever lists it.
#[test]
fn the_owner_adopts_the_listed_workspaces_once() {
    let store = store();
    let ann = store.add_user(&name("ann")).unwrap().id;
    store.set_workspace_owner(&ws("anns"), ann).unwrap();
    let listed = [ws("old"), ws("anns")];
    assert_eq!(
        store
            .adopt_unowned_workspaces(&crate::testing::all_users(), &listed)
            .unwrap(),
        1
    );
    assert_eq!(
        (
            store.workspace_owner(&ws("old")).unwrap(),
            store.workspace_owner(&ws("anns")).unwrap()
        ),
        (Some(UserId::OWNER), Some(ann))
    );
    assert_eq!(
        store
            .adopt_unowned_workspaces(&crate::testing::all_users(), &[ws("later")])
            .unwrap(),
        0
    );
    assert_eq!(store.workspace_owner(&ws("later")).unwrap(), None);
}

/// A database that cannot be written leaves the adoption to the next start.
#[test]
fn an_adoption_that_fails_is_tried_again() {
    let store = store();
    store.break_database(
        "CREATE TRIGGER no_owner BEFORE INSERT ON workspace_owners
         BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    );
    assert!(
        store
            .adopt_unowned_workspaces(&crate::testing::all_users(), &[ws("old")])
            .is_err()
    );
    store.break_database("DROP TRIGGER no_owner");
    assert_eq!(
        store
            .adopt_unowned_workspaces(&crate::testing::all_users(), &[ws("old")])
            .unwrap(),
        1
    );
}

#[test]
fn a_workspaces_user_is_recorded_once_and_never_changes() {
    let store = store();
    let ann = store.add_user(&name("ann")).unwrap().id;
    let bob = store.add_user(&name("bob")).unwrap().id;
    store.set_workspace_owner(&ws("a"), ann).unwrap();
    store.set_workspace_owner(&ws("a"), ann).unwrap();
    assert!(matches!(
        store.set_workspace_owner(&ws("a"), bob),
        Err(StoreError::WorkspaceOwned(_))
    ));
    assert!(matches!(
        store.set_workspace_owner(&ws("b"), UserId::from_stored(99)),
        Err(StoreError::UnknownUser(_))
    ));
    assert_eq!(store.workspace_owner(&ws("a")).unwrap(), Some(ann));
    let owners = store.workspace_owners().unwrap();
    assert_eq!(
        (
            owners.is(&ws("a"), ann),
            owners.is(&ws("a"), UserId::OWNER),
            owners.is(&ws("b"), ann),
        ),
        (true, false, false)
    );
}

#[test]
fn the_sign_in_secrets_are_handed_out_once() {
    let store = store();
    assert!(store.take_account_secrets().is_some());
    assert!(store.take_account_secrets().is_none());
}

#[test]
fn an_audit_record_carries_its_workspaces_user_and_a_filter_keeps_to_one_user() {
    let store = store();
    let ann = store.add_user(&name("ann")).unwrap().id;
    store.set_workspace_owner(&ws("annbox"), ann).unwrap();
    for workspace in ["annbox", "ownerbox"] {
        store
            .record_workspace_op(&NewWorkspaceOp {
                op: WorkspaceOpKind::Create,
                workspace: Some(ws(workspace)),
                user: None,
                outcome: WorkspaceOpOutcome::Succeeded,
                actor: Actor::Api,
                detail: None,
            })
            .unwrap();
    }
    let lines = |user: Option<UserId>| -> Vec<String> {
        let filter = AuditFilter {
            user,
            ..AuditFilter::default()
        };
        store
            .audit_query_every_user(
                &crate::testing::all_users(),
                &filter,
                AuditCursor::After(0),
                10,
            )
            .unwrap()
            .into_iter()
            .map(|(_, line)| line)
            .collect()
    };
    let anns = lines(Some(ann));
    assert_eq!(anns.len(), 1);
    assert!(anns[0].contains("annbox"), "{}", anns[0]);
    let owners = lines(Some(UserId::OWNER));
    assert_eq!(owners.len(), 1);
    assert!(owners[0].contains("ownerbox"), "{}", owners[0]);
    assert_eq!(lines(None).len(), 2);
}

/// What the user list (and the admin screen) is built from has exactly these fields: a new one
/// fails to compile here, so a credential cannot be added to it unnoticed.
#[test]
fn a_user_carries_no_credential() {
    let User {
        id,
        role,
        name,
        created_at,
    } = store().user(UserId::OWNER).unwrap();
    assert_eq!(
        (id, role, name.as_str(), created_at > 0),
        (UserId::OWNER, Role::Owner, "owner", true)
    );
}

/// A database that fails to read or write is an error, never a guess at the owner.
#[test]
fn a_failing_database_is_an_error() {
    let gone = store();
    gone.break_database("ALTER TABLE users RENAME TO users_gone");
    assert!(gone.add_user(&name("ann")).is_err());
    assert!(gone.set_workspace_owner(&ws("a"), UserId::OWNER).is_err());
    let unreadable = store();
    unreadable.break_database("ALTER TABLE workspace_owners RENAME TO owners_gone");
    assert!(
        unreadable
            .set_workspace_owner(&ws("a"), UserId::OWNER)
            .is_err()
    );
    let unwritable = store();
    unwritable.break_database(
        "CREATE TRIGGER no_owner BEFORE INSERT ON workspace_owners
         BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    );
    assert!(
        unwritable
            .set_workspace_owner(&ws("a"), UserId::OWNER)
            .is_err()
    );
}

#[test]
fn a_stored_role_that_is_unknown_is_corruption() {
    assert_eq!(role_of(3, "user").unwrap(), Role::User);
    assert!(matches!(
        role_of(3, "admin"),
        Err(StoreError::Corrupt {
            table: "users",
            id: 3,
            ..
        })
    ));
}
