// SPDX-License-Identifier: GPL-3.0-or-later
use std::sync::Arc;

use puddle_secrets::{AccountName, HostName, SourceSpec};

use super::*;
use crate::clock::ManualClock;
use crate::identity::{Coverage, Owner};
use crate::store::Limits;

fn store() -> Store {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    crate::testing::own(&store, &WorkspaceId::new("shop").unwrap());
    store
}

fn made(store: &Store) -> IdentityId {
    made_as(store, "Work")
}

fn made_as(store: &Store, label: &str) -> IdentityId {
    let host = HostName::new("github.com").unwrap();
    let source = SourceSpec::Gh {
        host: host.clone(),
        account: AccountName::new("me").unwrap(),
    };
    let covers = Coverage::new([Owner::new("acme").unwrap()].into(), false).unwrap();
    store
        .create_identity(
            UserId::OWNER,
            IdentityDraft {
                label: label.into(),
                author: Author::new("Me", "me@example.com").unwrap(),
                credentials: vec![CredentialBinding::new(&host, source, covers).unwrap()],
            },
        )
        .unwrap()
        .id
}

/// A row edited by hand to something that does not parse is reported, never trusted.
#[test]
fn a_hand_edited_identity_row_is_reported_as_corrupt() {
    let store = store();
    let id = made(&store);
    lock(&store.conn)
        .execute("UPDATE identities SET credentials = 'not json'", [])
        .unwrap();
    assert!(matches!(
        store.identity(UserId::OWNER, id),
        Err(StoreError::Corrupt {
            table: "identities",
            ..
        })
    ));
    assert!(store.identities(UserId::OWNER).is_err());
}

#[test]
fn a_hand_edited_repository_row_is_reported_as_corrupt() {
    let store = store();
    let ws = WorkspaceId::new("shop").unwrap();
    lock(&store.conn)
        .execute(
            "INSERT INTO workspace_repos (workspace_id, host, owner, repo, pull, push, created_at)
                 VALUES ('shop', 'github.com', 'acme', 'a/../b', 1, 1, 1)",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.workspace_git(&ws),
        Err(StoreError::Corrupt {
            table: "workspace_repos",
            ..
        })
    ));
}

#[test]
fn an_identity_listed_twice_is_refused() {
    let store = store();
    let id = made(&store);
    let ws = WorkspaceId::new("shop").unwrap();
    assert!(matches!(
        store.set_workspace_identities(&ws, &[id, id]),
        Err(StoreError::IdentityInvalid(_))
    ));
    assert_eq!(store.workspace_git(&ws).unwrap().identities.len(), 0);
}

const DISK_FULL: &str = "database error: disk full";

#[test]
fn a_hand_edited_signing_is_reported_as_corrupt() {
    let store = store();
    let id = made(&store);
    // The table's own check refuses other values; a hand edit gets past it.
    store.break_database(
        "PRAGMA ignore_check_constraints = ON;
             UPDATE identities SET signing = 'gpg';
             PRAGMA ignore_check_constraints = OFF",
    );
    let err = store.identity(UserId::OWNER, id).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("stored identities row {id} is invalid: unknown signing gpg")
    );
}

#[test]
fn the_workspaces_an_identity_is_on_are_listed() {
    let store = store();
    let id = made(&store);
    assert_eq!(store.identity_workspaces(id).unwrap(), []);
    let ws = WorkspaceId::new("shop").unwrap();
    store.attach_identity(&ws, id, None).unwrap();
    assert_eq!(store.identity_workspaces(id).unwrap(), [ws]);
}

#[test]
fn attachments_that_cannot_be_read_are_a_database_error() {
    let store = store();
    let id = made(&store);
    let ws = WorkspaceId::new("shop").unwrap();
    store.break_database("DROP TABLE workspace_identities");
    let expected = "database error: no such table: workspace_identities";
    assert_eq!(
        store.identity_workspaces(id).unwrap_err().to_string(),
        expected
    );
    assert_eq!(
        store.detach_identity(&ws, id).unwrap_err().to_string(),
        expected
    );
}

#[test]
fn an_identity_that_cannot_be_inserted_is_not_made() {
    let store = store();
    store.break_database(
        "CREATE TRIGGER identity_inserts_fail BEFORE INSERT ON identities
             BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    );
    let draft = IdentityDraft {
        label: "Work".into(),
        author: Author::new("Me", "me@example.com").unwrap(),
        credentials: vec![],
    };
    let err = store.create_identity(UserId::OWNER, draft).unwrap_err();
    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.identities(UserId::OWNER).unwrap(), []);
}

#[test]
fn an_identity_that_cannot_be_updated_keeps_what_it_had() {
    let store = store();
    let id = made(&store);
    let before = store.identity(UserId::OWNER, id).unwrap();
    store.break_database(
        "CREATE TRIGGER identity_updates_fail BEFORE UPDATE ON identities
             BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    );
    let draft = IdentityDraft {
        label: "Renamed".into(),
        author: before.author.clone(),
        credentials: before.credentials.clone(),
    };
    let err = store.update_identity(UserId::OWNER, id, draft).unwrap_err();
    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.identity(UserId::OWNER, id).unwrap(), before);
}

#[test]
fn deleting_an_identity_that_is_not_the_default_leaves_the_default_alone() {
    let store = store();
    let default = made(&store);
    let other = made_as(&store, "Personal");
    assert_eq!(store.delete_identity(UserId::OWNER, other).unwrap(), []);
    assert_eq!(
        store.default_identity(UserId::OWNER).unwrap().unwrap().id,
        default
    );
    assert_eq!(store.identities(UserId::OWNER).unwrap().len(), 1);
}

#[test]
fn detaching_an_identity_that_is_not_attached_is_refused() {
    let store = store();
    let id = made(&store);
    let ws = WorkspaceId::new("shop").unwrap();
    let err = store.detach_identity(&ws, id).unwrap_err();
    assert_eq!(err.to_string(), format!("no identity {id}"));
}
