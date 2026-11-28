// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_store::{Limits, ManualClock, Store, UserId, UserName};
use puddle_types::WorkspaceId;

use super::{NotOwned, OwnedWorkspace};
use crate::ports::{FakePorts, PortsService};

fn store() -> Store {
    Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap()
}

fn ws(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

#[test]
fn a_handle_exists_only_for_the_workspace_of_the_user_who_asks() {
    let store = store();
    let ann = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    store.set_workspace_owner(&ws("anns"), ann).unwrap();
    store
        .set_workspace_owner(&ws("owners"), UserId::OWNER)
        .unwrap();

    let mine = OwnedWorkspace::resolve(&store, ann, &ws("anns")).unwrap();
    assert_eq!(mine.id(), &ws("anns"));
    assert_eq!(mine.user(), ann);

    for (user, workspace) in [(ann, "owners"), (UserId::OWNER, "anns"), (ann, "nobodys")] {
        assert_eq!(
            OwnedWorkspace::resolve(&store, user, &ws(workspace))
                .unwrap_err()
                .to_string(),
            NotOwned.to_string(),
            "{user:?} {workspace}"
        );
    }
}

#[tokio::test]
async fn a_service_call_takes_the_handle_so_another_users_id_cannot_reach_it() {
    let store = store();
    let ann = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    store.set_workspace_owner(&ws("anns"), ann).unwrap();
    let ports = FakePorts::new();

    let mine = OwnedWorkspace::resolve(&store, ann, &ws("anns")).unwrap();
    ports.forward(&mine, 8080, None, false).await.unwrap();
    assert_eq!(ports.list(&mine).await.unwrap().ports.len(), 1);

    // The owner has no handle for ann's workspace, so no forward or release can be written for it.
    assert!(OwnedWorkspace::resolve(&store, UserId::OWNER, &ws("anns")).is_err());
}
