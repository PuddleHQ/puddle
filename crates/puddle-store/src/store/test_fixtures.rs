// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared setup of the store tests that name one workspace, `box`, and one global owner.

use puddle_types::{Host, PendingId, WorkspaceId};

use super::*;
use crate::clock::ManualClock;
use crate::rule::Effect;

pub(super) fn store_with(limits: Limits) -> (Arc<ManualClock>, Store) {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Store::open_in_memory(clock.clone(), limits).unwrap();
    crate::testing::own(&store, &workspace());
    (clock, store)
}

pub(super) fn store() -> Store {
    store_with(Limits::default()).1
}

pub(super) fn workspace() -> WorkspaceId {
    WorkspaceId::new("box").unwrap()
}

pub(super) fn request(host: &str) -> EgressRequest {
    EgressRequest::new(workspace(), Host::parse_normalised(host).unwrap(), 443)
}

pub(super) fn new_pending(store: &Store, host: &str) -> PendingId {
    let decision = store.decide(&request(host), SuffixAllows::Count).unwrap();
    assert!(matches!(
        decision,
        Decision::Pending(PendingOutcome::New(_))
    ));
    let open = store
        .open_pending_every_user(&crate::testing::all_users(), Some(&workspace()))
        .unwrap();
    open.into_iter()
        .find(|row| row.host.to_string() == host)
        .unwrap()
        .id
}

pub(super) fn workspace_rule(host: &str) -> NewRule {
    NewRule {
        scope: Scope::Workspace(workspace()),
        pattern: Pattern::parse(host).unwrap(),
        effect: Effect::Allow,
        expires_at: None,
        created_by: Actor::Cli,
    }
}
