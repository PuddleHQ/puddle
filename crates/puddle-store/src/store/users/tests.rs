// SPDX-License-Identifier: GPL-3.0-or-later
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
fn a_workspace_with_no_recorded_user_is_the_owners() {
    let store = store();
    assert_eq!(store.workspace_owner(&ws("box")).unwrap(), UserId::OWNER);
    assert_eq!(
        store.workspace_owners().unwrap().of(&ws("box")),
        UserId::OWNER
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
    assert_eq!(store.workspace_owner(&ws("a")).unwrap(), ann);
    let owners = store.workspace_owners().unwrap();
    assert_eq!(
        (owners.of(&ws("a")), owners.of(&ws("b"))),
        (ann, UserId::OWNER)
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
            .audit_query(&filter, AuditCursor::After(0), 10)
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
