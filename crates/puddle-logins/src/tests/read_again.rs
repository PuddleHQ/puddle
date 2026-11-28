// SPDX-License-Identifier: AGPL-3.0-or-later
//! A start the store did not answer: the first exchange of each service reads its login again.

use super::kept::marker_in;
use super::*;
use crate::test_rig::*;

/// A start where the store did not answer, then the store answers: the rig of the next start, and
/// the stand-in the workspace's disk still holds for the access token.
async fn start_with_the_store_down_after_a_login() -> (Rig, String) {
    let first = rig();
    let given = claude_login(&first, REAL_ACCESS, REAL_REFRESH).await;
    first.store.break_it();
    let next = rig_on(&first.store, true);
    next.logins.load().await;
    assert_eq!(next.registry.len(), 0);
    (next, given["access_token"].as_str().unwrap().to_owned())
}

/// What the proxy does first with a request: finishes the exchange's own work.
async fn first_exchange(rig: &Rig, token_host: &str, path: &str) {
    let mut exchange = rig
        .logins
        .begin(&host(token_host), "POST", path)
        .expect("the request is an exchange");
    exchange.prepare().await;
}

#[tokio::test]
async fn the_first_exchange_after_a_start_the_store_did_not_answer_reads_the_kept_login_again() {
    let (next, given) = start_with_the_store_down_after_a_login().await;
    next.store.heal();
    first_exchange(&next, "platform.claude.com", "/v1/oauth/token").await;
    assert_eq!(next.registry.len(), 2);
    // The sign-in that follows reuses the kept stand-ins and does not write over the login.
    let again = claude_login(&next, ACCESS_B, REFRESH_B).await;
    assert_eq!(again["access_token"], given);
    assert_eq!(next.registry.len(), 2);
    assert_eq!(
        vault(&next)
            .read(&CLAUDE, Role::Access)
            .unwrap()
            .unwrap()
            .real
            .as_str(),
        ACCESS_B
    );
}

#[tokio::test]
async fn a_service_is_read_again_once_even_when_the_store_still_does_not_answer() {
    let (next, _) = start_with_the_store_down_after_a_login().await;
    first_exchange(&next, "platform.claude.com", "/v1/oauth/token").await;
    assert_eq!(next.registry.len(), 0);
    next.store.heal();
    first_exchange(&next, "platform.claude.com", "/v1/oauth/token").await;
    assert_eq!(next.registry.len(), 0, "one attempt for each service");
}

#[tokio::test]
async fn an_exchange_reads_again_only_its_own_service() {
    let first = rig();
    claude_login(&first, REAL_ACCESS, REAL_REFRESH).await;
    let github = first
        .logins
        .begin(&host("github.com"), "POST", "/login/oauth/access_token");
    answered_bytes(
        github,
        200,
        Some("application/x-www-form-urlencoded"),
        format!("access_token={REAL_GH}&scope=repo&token_type=bearer").as_bytes(),
    )
    .await
    .unwrap();
    first.store.break_it();
    let next = rig_on(&first.store, true);
    next.logins.load().await;
    next.store.heal();
    first_exchange(&next, "platform.claude.com", "/v1/oauth/token").await;
    assert_eq!(next.registry.len(), 2, "claude's two tokens, not GitHub's");
    first_exchange(&next, "github.com", "/login/oauth/access_token").await;
    assert_eq!(next.registry.len(), 3);
}

#[tokio::test]
async fn a_start_the_store_answered_has_nothing_to_read_again() {
    let first = rig();
    claude_login(&first, REAL_ACCESS, REAL_REFRESH).await;
    let next = rig_on(&first.store, true);
    next.logins.load().await;
    assert_eq!(next.registry.len(), 2);
    let mut exchange = next
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token")
        .unwrap();
    // Work the exchange asked for would show as a changed registry; none is asked for.
    first.store.break_it();
    exchange.prepare().await;
    assert_eq!(next.registry.len(), 2);
    assert_eq!(next.notices.seen(), []);
}

#[tokio::test]
async fn the_login_read_again_is_noted_in_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    rig_marked(&store, true, marker_in(&dir))
        .logins
        .load()
        .await;
    claude_login(&rig_on(&store, true), REAL_ACCESS, REAL_REFRESH).await;
    store.break_it();
    let next = rig_marked(&store, true, marker_in(&dir));
    next.logins.load().await;
    assert_eq!(next.logins.inner_marker_kept(), Some(vec![]));
    store.heal();
    first_exchange(&next, "platform.claude.com", "/v1/oauth/token").await;
    assert_eq!(
        next.logins.inner_marker_kept(),
        Some(vec!["claude".to_owned()])
    );
}

#[tokio::test]
async fn a_service_with_nothing_kept_is_not_noted_when_it_is_read_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    store.break_it();
    let next = rig_marked(&store, true, marker_in(&dir));
    next.logins.load().await;
    store.heal();
    first_exchange(&next, "platform.claude.com", "/v1/oauth/token").await;
    assert_eq!(next.registry.len(), 0);
    assert!(
        next.notices
            .seen()
            .iter()
            .all(|(_, p)| *p == Problem::StoreUnavailable)
    );
}

#[tokio::test]
async fn a_damaged_entry_found_when_read_again_is_reported_as_unreadable() {
    let (next, _) = start_with_the_store_down_after_a_login().await;
    next.store.heal();
    let id = crate::vault::slot_id("alpha", "claude", Role::Refresh).unwrap();
    next.store
        .set(&id, &puddle_secrets::Secret::new("not json".into()))
        .unwrap();
    first_exchange(&next, "platform.claude.com", "/v1/oauth/token").await;
    assert_eq!(next.registry.len(), 1, "the access slot is back");
    assert_eq!(
        next.notices.seen().last(),
        Some(&("claude".to_owned(), Problem::Unreadable))
    );
}

#[tokio::test]
async fn a_slot_that_holds_a_stand_in_already_is_not_read_again() {
    let (next, _) = start_with_the_store_down_after_a_login().await;
    next.store.heal();
    let kept = claude_login(&next, ACCESS_B, REFRESH_B).await;
    // The store now holds another pair than the one the slot was given.
    vault(&next)
        .write(
            &CLAUDE,
            Role::Access,
            "puddle-other-stand-in-0123456789abcdef",
            REAL_ACCESS,
        )
        .unwrap();
    next.logins.read_again(&CLAUDE).await;
    assert_eq!(next.registry.len(), 2);
    let again = claude_login(&next, ACCESS_B, REFRESH_B).await;
    assert_eq!(again["access_token"], kept["access_token"]);
}
