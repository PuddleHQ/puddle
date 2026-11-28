// SPDX-License-Identifier: AGPL-3.0-or-later
//! Session tokens that are exchanged again and again (Copilot) and the stand-ins they leave
//! swapped.

use super::*;
use crate::test_rig::*;

const SESSION_FIELDS: &str = "tid=7a1b;exp=1790000000;sku=free_limited_copilot;proxy-ep=proxy.individual.githubcopilot.com;chat=1";
const MAC_1: &str = "0badc0ffee0badc0ffee0badc0ffee0badc0ffee0badc0ffee0badc0ffee0bad";

fn session_token(n: usize) -> String {
    // A different real token at every exchange: its own tid and its own MAC.
    let mac = format!("{n:x}{}", &MAC_1[1..]);
    format!("{SESSION_FIELDS};n={n}:{mac}")
}

fn session_answer(token: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "token": token,
        "expires_at": 1_790_000_000,
        "refresh_in": 1500,
        "sku": "free_limited_copilot",
        "endpoints": {"api": "https://api.individual.githubcopilot.com"},
    }))
    .unwrap()
}

/// The extension's exchange: a request to the session token endpoint with `method`.
async fn copilot_exchange(rig: &Rig, method: &str, token: &str) -> Option<Value> {
    let exchange = rig.logins.begin(
        &host("api.github.com"),
        method,
        "/copilot_internal/v2/token",
    );
    given_or_refused(
        rewritten(
            exchange,
            200,
            Some("application/json"),
            &session_answer(token),
        )
        .await,
    )
}

#[tokio::test]
async fn a_copilot_session_token_is_captured_for_any_method_and_the_guest_gets_a_stand_in_with_its_fields()
 {
    for method in ["GET", "POST", "PUT"] {
        let rig = rig();
        let real = session_token(1);
        let given = copilot_exchange(&rig, method, &real)
            .await
            .unwrap_or_else(|| panic!("{method} is captured"));
        let stand_in = given["token"].as_str().unwrap();
        let (fields, mac) = stand_in.split_once(':').unwrap();
        assert_eq!(fields, format!("{SESSION_FIELDS};n=1"), "{method}");
        assert_eq!(mac.len(), MAC_1.len());
        assert!(
            !given.to_string().contains(&MAC_1[1..]),
            "{method}: {given}"
        );
        assert_eq!(given["refresh_in"], 1500);
        assert_eq!(given["sku"], "free_limited_copilot");
        assert_eq!(rig.registry.len(), 1);
        assert_eq!(rig.notices.seen(), []);
        let kept = vault(&rig).read(&GITHUB, Role::Copilot).unwrap().unwrap();
        assert_eq!(
            (kept.stand_in.as_str(), kept.real.as_str()),
            (stand_in, real.as_str())
        );
        assert_eq!(rig.logins.kept()[0].roles, [Role::Copilot]);
    }
}

#[tokio::test]
async fn other_requests_to_the_github_api_host_are_not_exchanges() {
    let rig = rig();
    for (path, method) in [
        ("/copilot_internal/user", "GET"),
        ("/copilot_internal/v2/nltoken", "GET"),
        ("/user", "GET"),
    ] {
        assert!(
            rig.logins
                .begin(&host("api.github.com"), method, path)
                .is_none(),
            "{method} {path}"
        );
    }
    // The login endpoint of the same profile still reads only a `POST`.
    assert!(
        rig.logins
            .begin(&host("github.com"), "GET", "/login/oauth/access_token")
            .is_none()
    );
    assert!(
        rig.logins
            .begin(&host("github.com"), "POST", "/login/oauth/access_token")
            .is_some()
    );
    // With capture off no method is read.
    let off = rig_on(&Arc::new(MemoryStore::new()), false);
    assert!(
        off.logins
            .begin(&host("api.github.com"), "GET", "/copilot_internal/v2/token")
            .is_none()
    );
}

#[tokio::test]
async fn an_answer_found_too_large_while_it_streams_is_refused_and_the_user_is_told() {
    let rig = rig();
    let mut exchange = rig
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token")
        .unwrap();
    let answer = exchange.answer_mut().unwrap();
    let Rewrite::Refuse(refusal) = answer.too_large() else {
        panic!("the answer is refused");
    };
    assert_eq!(refusal.code(), ReasonCode::LoginNotKept);
    assert_eq!(
        rig.notices.seen(),
        [("claude".to_owned(), Problem::UnexpectedAnswer)]
    );
}

#[tokio::test]
async fn every_session_token_exchange_gets_a_new_stand_in_and_the_last_few_stay_swapped() {
    let rig = rig();
    let mut given = Vec::new();
    for n in 1..=6 {
        let answer = copilot_exchange(&rig, "GET", &session_token(n))
            .await
            .unwrap();
        given.push(answer["token"].as_str().unwrap().to_owned());
    }
    for pair in given.windows(2) {
        assert_ne!(pair[0], pair[1]);
    }
    // The newest and the three before it are swapped; the two oldest are not.
    assert_eq!(rig.registry.len(), 4);
    let held = rig.registry.ids();
    assert_eq!(held, ["stand-in:login:github-copilot"; 4]);
    let kept = vault(&rig).read(&GITHUB, Role::Copilot).unwrap().unwrap();
    assert_eq!(kept.stand_in, given[5]);
    assert_eq!(kept.real.as_str(), session_token(6));
    assert_eq!(rig.logins.kept().len(), 1);
}

#[tokio::test]
async fn a_long_run_of_session_exchanges_never_holds_more_than_the_bound() {
    let rig = rig();
    for n in 1..=60 {
        copilot_exchange(&rig, "GET", &session_token(n))
            .await
            .unwrap();
        assert!(
            rig.registry.len() <= 1 + crate::logins::RETIRED_KEPT,
            "exchange {n}"
        );
    }
    assert_eq!(rig.registry.len(), 1 + crate::logins::RETIRED_KEPT);
    assert_eq!(rig.logins.forget("github").await, Ok(true));
    assert_eq!(rig.registry.len(), 0);
}

#[tokio::test]
async fn forgetting_the_github_login_drops_every_session_stand_in() {
    let rig = rig();
    for n in 1..=3 {
        copilot_exchange(&rig, "GET", &session_token(n))
            .await
            .unwrap();
    }
    assert_eq!(rig.registry.len(), 3);
    assert_eq!(rig.logins.forget("github").await, Ok(true));
    assert_eq!(rig.registry.len(), 0);
    assert!(vault(&rig).read(&GITHUB, Role::Copilot).unwrap().is_none());
}

#[tokio::test]
async fn a_restart_gets_the_last_session_stand_in_back() {
    let first = rig();
    let given = copilot_exchange(&first, "GET", &session_token(1))
        .await
        .unwrap();
    let next = rig_on(&first.store, true);
    next.logins.load().await;
    assert_eq!(next.registry.len(), 1);
    assert_eq!(next.notices.seen(), []);
    let kept = vault(&next).read(&GITHUB, Role::Copilot).unwrap().unwrap();
    assert_eq!(kept.stand_in, given["token"].as_str().unwrap());
}

#[tokio::test(start_paused = true)]
async fn hostile_a_session_token_without_a_mac_is_never_given_to_the_guest_and_the_user_is_told() {
    let rig = rig();
    for token in [
        "tid=1;exp=2;sku=free_limited_copilot;chat=1",
        "not-fields-and-no-colon-0123456789abcdef",
    ] {
        // each in its own notice window
        tokio::time::advance(NOTICE_WINDOW).await;
        let kept = copilot_exchange(&rig, "GET", token).await;
        assert!(kept.is_none(), "{token:?} is not replaced");
    }
    // An empty token holds nothing real, so the answer goes through as sent.
    tokio::time::advance(NOTICE_WINDOW).await;
    let exchange = rig
        .logins
        .begin(&host("api.github.com"), "GET", "/copilot_internal/v2/token");
    let empty = rewritten(exchange, 200, Some("application/json"), &session_answer("")).await;
    assert_eq!(empty, Rewrite::Keep);
    assert_eq!(rig.registry.len(), 0, "nothing was registered");
    assert!(vault(&rig).read(&GITHUB, Role::Copilot).unwrap().is_none());
    let seen = rig.notices.seen();
    assert!(
        seen.iter()
            .all(|(profile, problem)| profile == "github" && *problem == Problem::UnusableToken),
        "{seen:?}"
    );
    assert_eq!(seen.len(), 2, "the empty token is not a token");
}
