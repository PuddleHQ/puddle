// SPDX-License-Identifier: GPL-3.0-or-later
use puddle_store::{Limits, ManualClock};

use super::*;
use crate::workspaces::{RepoUrl, Repository};

fn at(url: &str, identity: Option<i64>) -> NewRepository {
    NewRepository {
        repository: Repository::at_default_directory(
            crate::new_entry_id(|_| false).unwrap(),
            RepoUrl::parse(url).unwrap(),
        ),
        identity: identity.map(IdentityId),
        pull: true,
        push: true,
        local: None,
    }
}

#[test]
fn a_list_the_store_cannot_set_up_is_cloned_without_and_every_entry_says_why() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    let ws = WorkspaceId::new("w").unwrap();
    puddle_store::testing::own(&store, &ws);
    // The second repository's identity is gone by the time the create sets it up: the first
    // is not set up either, as the list is one transaction.
    let list = [
        at("https://github.com/acme/web", None),
        at("https://github.com/acme/api", Some(42)),
    ];
    let answers = start_all(&store, &ws, &list);
    assert_eq!(answers.len(), 2);
    for (answer, url) in answers
        .iter()
        .zip(["https://github.com/acme/web", "https://github.com/acme/api"])
    {
        assert_eq!(answer.identity, None);
        let warning = answer.warning.as_deref().unwrap();
        assert!(
            warning.starts_with(&format!(
                "Puddle could not set up the Git settings of {url} ("
            )),
            "{warning}"
        );
        assert!(warning.ends_with("Set them on the Git tab."), "{warning}");
    }
    assert_eq!(store.workspace_git(&ws).unwrap().repos, []);
}

#[test]
fn a_repository_the_table_cannot_hold_is_skipped_and_the_rest_are_set_up() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    let ws = WorkspaceId::new("w").unwrap();
    puddle_store::testing::own(&store, &ws);
    let list = [
        at("https://github.com/a/b/c", None),
        at("https://github.com/acme/web", None),
    ];
    let answers = start_all(&store, &ws, &list);
    assert!(
        answers[0]
            .warning
            .as_deref()
            .unwrap()
            .contains("cannot tell which repository https://github.com/a/b/c is")
    );
    // The second was set up: its notice is about the missing identity, not a failed set-up.
    assert!(
        answers[1]
            .warning
            .as_deref()
            .unwrap()
            .starts_with("No identity covers github.com/acme")
    );
    assert_eq!(store.workspace_git(&ws).unwrap().repos.len(), 1);
}

/// A create records whose the new workspace is, the owner's included, and a record that cannot be
/// written stops the create.
#[test]
fn a_create_claims_the_workspace_for_its_user() {
    let store =
        Arc::new(Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap());
    let ann = store
        .add_user(&puddle_store::UserName::new("ann").unwrap())
        .unwrap()
        .id;
    let id = |text: &str| WorkspaceId::new(text).unwrap();
    StoreGitSetup::new(store.clone())
        .claim(&id("owners"), UserId::OWNER)
        .unwrap();
    StoreGitSetup::new(store.clone())
        .claim(&id("anns"), ann)
        .unwrap();
    assert_eq!(
        (
            store.workspace_owner(&id("owners")).unwrap(),
            store.workspace_owner(&id("anns")).unwrap(),
        ),
        (Some(UserId::OWNER), Some(ann))
    );
    // Another user's id (a workspace's user never changes) and an unknown user.
    let taken = StoreGitSetup::new(store.clone())
        .claim(&id("anns"), UserId::from_stored(ann.get() + 1))
        .unwrap_err();
    assert!(matches!(taken, WorkspaceError::Unavailable(_)), "{taken:?}");
    store
        .add_user(&puddle_store::UserName::new("bo").unwrap())
        .unwrap();
    let taken = StoreGitSetup::new(store.clone())
        .claim(&id("anns"), UserId::from_stored(ann.get() + 1))
        .unwrap_err();
    assert!(matches!(taken, WorkspaceError::Conflict(_)), "{taken:?}");
    let taken = StoreGitSetup::new(store.clone())
        .claim(&id("owners"), ann)
        .unwrap_err();
    assert!(matches!(taken, WorkspaceError::Conflict(_)), "{taken:?}");
}
