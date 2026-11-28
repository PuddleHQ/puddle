// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decisions about the notices a refusal raises and the memory behind them.

use super::*;

const PRIVATE: &str = "/someone/private.git/info/refs?service=git-upload-pack";

/// Lets `by` pass in half-second steps, so the notice tasks that were due run in their turn.
async fn later(by: Duration) {
    for _ in 0..by.as_millis() / 500 {
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
    }
}

/// The server answers the request `decision` stands for with `401`.
fn answered_401(decision: &InjectDecision) {
    let InjectDecision::PassThroughNoting(unauthorized) = decision else {
        unreachable!("{decision:?} does not note a 401");
    };
    assert!(
        unauthorized.refusal().is_none(),
        "the server's answer stands"
    );
}

#[tokio::test]
async fn a_401_nobody_answers_tells_the_user_no_identity_covers_the_owner() {
    tokio::time::pause();
    let w = World::new();
    w.identity("Work", "github.com", &["acme"], false, "CANARY-WORK");
    let decision = w.decide("github.com", "GET", PRIVATE, &[]).await;
    answered_401(&decision);
    // The tool may still retry with a credential of its own: nothing yet.
    later(Duration::from_secs(1)).await;
    assert_eq!(w.events.take(), []);
    later(Duration::from_secs(4)).await;
    assert_eq!(
        w.events.take(),
        [Event::GitNoIdentity {
            workspace: w.workspace.clone(),
            host: "github.com".into(),
            owner: "someone".into(),
            repo: "private".into(),
        }]
    );
    // The same host again is the same notice (whatever the owner), and a later one is a new one.
    for _ in 0..3 {
        answered_401(&w.decide("github.com", "GET", PRIVATE, &[]).await);
    }
    later(Duration::from_secs(4)).await;
    assert_eq!(w.events.take(), []);
    later(Duration::from_secs(11)).await;
    answered_401(&w.decide("github.com", "GET", PRIVATE, &[]).await);
    later(Duration::from_secs(4)).await;
    assert_eq!(w.events.take().len(), 1);
}

#[test]
fn a_401_noted_outside_any_runtime_cannot_start_the_timer_and_says_nothing() {
    let w = World::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let decision = runtime.block_on(w.decide("github.com", "GET", PRIVATE, &[]));
    // The proxy always runs inside one; with none, the note is dropped rather than a panic.
    answered_401(&decision);
    assert_eq!(w.events.take(), []);
}

#[tokio::test]
async fn a_flood_of_owners_with_their_own_credential_is_remembered_in_bounded_memory() {
    tokio::time::pause();
    let w = World::new();
    for i in 0..600 {
        let target = format!("/owner{i}/repo.git/info/refs?service=git-upload-pack");
        let decision = w.decide("github.com", "GET", &target, &[OWN]).await;
        assert_eq!(decision, InjectDecision::PassThroughOwnCredential);
        if i % 100 == 0 {
            later(Duration::from_secs(5)).await;
        }
    }
    // A tool that did retry lately is still told apart from one that did not.
    answered_401(&w.decide("github.com", "GET", PRIVATE, &[]).await);
    later(Duration::from_secs(4)).await;
    assert_eq!(w.events.take().len(), 1);
}

#[tokio::test]
async fn a_workspace_cycling_through_owners_is_one_notice_per_host() {
    tokio::time::pause();
    let w = World::new();
    for owner in ["a", "b", "c", "d"] {
        let target = format!("/{owner}/repo.git/info/refs?service=git-upload-pack");
        answered_401(&w.decide("github.com", "GET", &target, &[]).await);
    }
    later(Duration::from_secs(5)).await;
    assert_eq!(w.events.take().len(), 1);
}

#[tokio::test]
async fn a_tool_that_retries_with_its_own_credential_is_not_told_off() {
    tokio::time::pause();
    let w = World::new();
    answered_401(&w.decide("github.com", "GET", PRIVATE, &[]).await);
    later(Duration::from_millis(50)).await;
    // A token from a credential helper, `.netrc` or the address goes out as the tool sent it.
    let retry = w.decide("github.com", "GET", PRIVATE, &[OWN]).await;
    assert_eq!(retry, InjectDecision::PassThroughOwnCredential);
    later(Duration::from_secs(10)).await;
    assert_eq!(w.events.take(), []);
    // Another owner's retry does not cover this one.
    answered_401(
        &w.decide(
            "github.com",
            "GET",
            "/other/repo.git/info/refs?service=git-upload-pack",
            &[],
        )
        .await,
    );
    let _ = w.decide("github.com", "GET", PRIVATE, &[OWN]).await;
    later(Duration::from_secs(10)).await;
    assert_eq!(w.events.take().len(), 1);
}

// The bounds of the notice memory: an entry counts for exactly ten seconds, and a table full of
// recent entries takes no more.

#[tokio::test]
async fn a_refusal_is_a_new_notice_exactly_when_ten_seconds_have_passed() {
    tokio::time::pause();
    let w = world();
    let target = "/acme/other.git/git-receive-pack";
    let _ = w.decide("github.com", "POST", target, &[]).await;
    assert_eq!(w.events.take().len(), 1);
    tokio::time::advance(Duration::from_millis(9_999)).await;
    let _ = w.decide("github.com", "POST", target, &[]).await;
    assert_eq!(w.events.take(), [], "still the same notice");
    tokio::time::advance(Duration::from_millis(1)).await;
    let _ = w.decide("github.com", "POST", target, &[]).await;
    assert_eq!(w.events.take().len(), 1, "ten seconds on, it is new");
}

#[tokio::test]
async fn a_full_notice_table_makes_room_exactly_when_its_entries_are_ten_seconds_old() {
    tokio::time::pause();
    let w = world();
    for n in 0..256 {
        let target = format!("/acme/other{n}.git/git-receive-pack");
        let _ = w.decide("github.com", "POST", &target, &[]).await;
    }
    assert_eq!(w.events.take().len(), 256);
    tokio::time::advance(Duration::from_millis(9_999)).await;
    let _ = w
        .decide("github.com", "POST", "/acme/late.git/git-receive-pack", &[])
        .await;
    assert_eq!(w.events.take(), [], "nothing old enough to forget yet");
    tokio::time::advance(Duration::from_millis(1)).await;
    let _ = w
        .decide("github.com", "POST", "/acme/late.git/git-receive-pack", &[])
        .await;
    assert_eq!(w.events.take().len(), 1, "ten seconds on, room is made");
}

/// 256 owners send their own credential at once, then after `wait` a tool that was refused a
/// credential by a `401` retries with its own: how many "no identity covers it" events follow.
async fn no_identity_events_after_a_full_table(wait: Duration) -> usize {
    let w = World::new();
    for i in 0..256 {
        let target = format!("/owner{i}/repo.git/info/refs?service=git-upload-pack");
        let decision = w.decide("github.com", "GET", &target, &[OWN]).await;
        assert_eq!(decision, InjectDecision::PassThroughOwnCredential);
    }
    tokio::time::advance(wait).await;
    answered_401(&w.decide("github.com", "GET", PRIVATE, &[]).await);
    let _ = w.decide("github.com", "GET", PRIVATE, &[OWN]).await;
    later(Duration::from_secs(5)).await;
    w.events.take().len()
}

#[tokio::test]
async fn a_retry_is_remembered_even_while_the_table_is_full_of_recent_owners() {
    tokio::time::pause();
    // The oldest entry makes room, so the retry is remembered and no notice follows, however
    // young the other entries are.
    for wait in [
        Duration::ZERO,
        Duration::from_secs(5),
        Duration::from_millis(9_999),
        Duration::from_secs(10),
        Duration::from_secs(11),
    ] {
        assert_eq!(
            no_identity_events_after_a_full_table(wait).await,
            0,
            "{wait:?}"
        );
    }
}
