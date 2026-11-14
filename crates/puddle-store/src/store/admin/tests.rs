// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;
use crate::clock::ManualClock;
use crate::store::Limits;

fn setup() -> (Store, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(1_000));
    let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
    (store, clock)
}

fn add(store: &Store, name: &str) -> UserId {
    store
        .admin_add_user(UserId::OWNER, &UserName::new(name).unwrap())
        .unwrap()
        .id
}

/// Gives `user` one row in every table that holds a user's own things.
fn seed(store: &Store, user: UserId, workspace: &WorkspaceId) {
    let conn = lock(&store.conn);
    let uid = user.get();
    for sql in [
        "INSERT INTO identities (user_id, label, author_name, author_email, credentials, position, is_default, created_at, changed_at)
             VALUES (?1, 'WORK', 'n', 'e@x', '[]', 0, 1, 1, 1)",
        "INSERT INTO rule_sets (user_id, name, created_at, created_by) VALUES (?1, 'mine', 1, 'ui')",
        "INSERT INTO env_vars (user_id, workspace_id, name, kind, value, changed_at) VALUES (?1, NULL, 'A', 'plain', 'v', 1)",
        "INSERT INTO git_defaults (user_id, only_push_listed, only_pull_listed) VALUES (?1, 1, 0)",
    ] {
        conn.execute(sql, [uid]).unwrap();
    }
    conn.execute(
        "INSERT INTO workspace_owners (workspace_id, user_id) VALUES (?1, ?2)",
        params![workspace.as_str(), uid],
    )
    .unwrap();
}

#[test]
fn adding_a_user_is_audited_and_a_taken_name_is_refused() {
    let (store, _) = setup();
    let ann = add(&store, "ann");
    assert!(
        store
            .admin_add_user(UserId::OWNER, &UserName::new("ANN").unwrap())
            .is_err()
    );
    let lines = store.admin_audit(10).unwrap();
    assert_eq!(lines.len(), 1, "a refused add leaves no line");
    assert_eq!(lines[0].action, AdminAction::UserCreated);
    assert_eq!(
        (lines[0].target, lines[0].target_name.as_str()),
        (ann, "ann")
    );
    assert_eq!(lines[0].actor, UserId::OWNER);
}

#[test]
fn the_user_list_counts_devices_that_work_and_dates_the_last_sign_in() {
    let (store, clock) = setup();
    let ann = add(&store, "ann");
    let first = store.add_device(ann, "a", 50_000).unwrap();
    clock.advance(10);
    store.add_device(ann, "b", 50_000).unwrap();
    store.revoke_device(ann, first.id).unwrap();
    let facts = store.user_facts().unwrap();
    assert_eq!(facts[0].user.id, UserId::OWNER, "the owner is first");
    assert_eq!((facts[0].devices, facts[0].last_sign_in), (0, None));
    assert_eq!((facts[1].devices, facts[1].last_sign_in), (1, Some(1_010)));
    assert_eq!(facts[1].deleting_since, None);
}

#[test]
fn deleting_a_user_removes_every_row_that_names_them_and_nobody_elses() {
    let (store, _) = setup();
    let ann = add(&store, "ann");
    let bob = add(&store, "bob");
    let (ws_a, ws_b) = (
        WorkspaceId::new("ann-box").unwrap(),
        WorkspaceId::new("bob-box").unwrap(),
    );
    seed(&store, ann, &ws_a);
    seed(&store, bob, &ws_b);
    store.add_device(ann, "phone", 50_000).unwrap();
    assert_eq!(store.workspaces_of(ann).unwrap(), vec![ws_a.clone()]);

    store.begin_user_delete(UserId::OWNER, ann).unwrap();
    assert_eq!(store.users_deleting().unwrap().len(), 1);
    store.finish_user_delete(ann).unwrap();

    assert!(matches!(store.user(ann), Err(StoreError::UnknownUser(_))));

    // Every table with a `user_id` column holds nothing of Ann's, and everything of Bob's.
    let conn = lock(&store.conn);
    let tables: Vec<String> = conn
        .prepare("SELECT m.name FROM sqlite_master m, pragma_table_info(m.name) c WHERE m.type = 'table' AND c.name = 'user_id'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(tables.len() >= 9, "{tables:?}");
    for table in &tables {
        let count = |user: UserId| -> i64 {
            conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE user_id = ?1"),
                [user.get()],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            count(ann),
            0,
            "{table} still holds rows of the deleted user"
        );
        if table != "devices" && table != "account_passwords" && table != "account_invites" {
            assert!(
                count(bob) > 0
                    || table == "rules"
                    || table == "request_rules"
                    || table == "audit"
                    || table == "rule_set_switches",
                "{table} lost Bob's rows"
            );
        }
    }
    drop(conn);
    let lines = store.admin_audit(10).unwrap();
    let actions: Vec<_> = lines.iter().map(|l| l.action).collect();
    assert_eq!(
        actions,
        [
            AdminAction::UserDeleted,
            AdminAction::UserDeleteStarted,
            AdminAction::UserCreated,
            AdminAction::UserCreated
        ]
    );
    assert_eq!(lines[0].target_name, "ann", "the audit outlives the user");
}

#[test]
fn deleting_begins_once_refuses_the_owner_and_finishes_only_what_began() {
    let (store, clock) = setup();
    let ann = add(&store, "ann");
    assert!(matches!(
        store.begin_user_delete(UserId::OWNER, UserId::OWNER),
        Err(StoreError::OwnerKept)
    ));
    assert!(matches!(
        store.finish_user_delete(ann),
        Err(StoreError::UnknownUser(_))
    ));
    store.begin_user_delete(UserId::OWNER, ann).unwrap();
    clock.advance(5);
    store.begin_user_delete(UserId::OWNER, ann).unwrap();
    let lines = store.admin_audit(10).unwrap();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.action == AdminAction::UserDeleteStarted)
            .count(),
        1,
        "a second begin writes no second line"
    );
    let facts = store.user_facts().unwrap();
    assert_eq!(facts[1].deleting_since, Some(1_000));
    assert!(matches!(
        store.finish_user_delete(UserId::OWNER),
        Err(StoreError::UnknownUser(_))
    ));
    assert!(matches!(
        store.begin_user_delete(UserId::OWNER, UserId::from_stored(99)),
        Err(StoreError::UnknownUser(_))
    ));
}

#[test]
fn the_audit_reads_newest_first_and_stops_at_the_limit() {
    let (store, _) = setup();
    let ann = add(&store, "ann");
    store.note_invite_reissued(UserId::OWNER, ann).unwrap();
    let one = store.admin_audit(1).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].action, AdminAction::InviteReissued);
    assert!(matches!(
        store.note_invite_reissued(UserId::OWNER, UserId::from_stored(99)),
        Err(StoreError::UnknownUser(_))
    ));
}

#[test]
fn a_database_that_fails_leaves_no_half_made_user_and_no_half_written_audit() {
    let name = UserName::new("bo").unwrap();
    // The name check cannot read the users.
    let (store, _) = setup();
    store.break_database("ALTER TABLE users RENAME COLUMN name TO name_gone");
    assert!(store.admin_add_user(UserId::OWNER, &name).is_err());

    // The audit cannot be written: each step that audits fails whole.
    let (store, _) = setup();
    let ann = add(&store, "ann");
    store.break_database("ALTER TABLE admin_audit RENAME TO audit_gone");
    assert!(store.note_invite_reissued(UserId::OWNER, ann).is_err());
    assert!(store.begin_user_delete(UserId::OWNER, ann).is_err());
    assert!(store.users_deleting().unwrap().is_empty(), "no half start");
    // A deletion that began before the audit broke is not finished either.
    let (store, _) = setup();
    let bob = add(&store, "bob");
    store.begin_user_delete(UserId::OWNER, bob).unwrap();
    store.break_database("ALTER TABLE admin_audit RENAME TO audit_gone");
    assert!(store.finish_user_delete(bob).is_err());
    assert_eq!(store.users_deleting().unwrap().len(), 1, "still to finish");
}
