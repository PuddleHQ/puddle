// SPDX-License-Identifier: AGPL-3.0-or-later
//! The group that runs real `puddle-api` code (the guard, approval, rule expiry) behind the fixture.
//! Every other group in this folder reads back what the fixture seeded or a control step set, so
//! it tests the fixture; a test here fails when the product route it names breaks.

use super::*;

#[tokio::test]
async fn the_api_keeps_its_guard() {
    let run = start("default").await;
    let host = format!("127.0.0.1:{}", run.api.port());
    let no_token = http(run.api, &host, None, "GET", "/api/pending", None).await;
    assert_eq!(no_token.status, 401);
    let wrong_host = http(
        run.api,
        "evil.example",
        Some(&run.token),
        "GET",
        "/api/pending",
        None,
    )
    .await;
    assert_eq!(wrong_host.status, 421);
    // The control server wants the token too.
    let host = format!("127.0.0.1:{}", run.control.port());
    let no_token = http(run.control, &host, None, "GET", "/control/state", None).await;
    assert_eq!(no_token.status, 401);
}

#[tokio::test]
async fn a_wrong_token_is_refused_by_the_api_and_the_control_server() {
    let run = start("default").await;
    // The same length as the real token, so only the comparison can tell them apart.
    let wrong = "x".repeat(run.token.len());
    for token in [wrong.as_str(), "", "wrong"] {
        let host = format!("127.0.0.1:{}", run.api.port());
        let reply = http(run.api, &host, Some(token), "GET", "/api/pending", None).await;
        assert_eq!(reply.status, 401, "api, token {token:?}");
        let host = format!("127.0.0.1:{}", run.control.port());
        let reply = http(
            run.control,
            &host,
            Some(token),
            "GET",
            "/control/state",
            None,
        )
        .await;
        assert_eq!(reply.status, 401, "control, token {token:?}");
    }
    // A refused reset changes nothing.
    let host = format!("127.0.0.1:{}", run.control.port());
    let reset = http(
        run.control,
        &host,
        Some("wrong"),
        "POST",
        "/control/reset",
        Some(&json!({"scenario": "empty"})),
    )
    .await;
    assert_eq!(reset.status, 401);
    assert_eq!(run.state().await["scenario"], "default");
}

#[tokio::test]
async fn the_control_server_refuses_a_host_that_is_not_its_own_address() {
    let run = start("default").await;
    let own = run.control.port();
    for host in [
        "evil.example".to_owned(),
        format!("evil.example:{own}"),
        // The API's address is not the control server's.
        format!("127.0.0.1:{}", run.api.port()),
    ] {
        let reset = http(
            run.control,
            &host,
            Some(&run.token),
            "POST",
            "/control/reset",
            Some(&json!({"scenario": "empty"})),
        )
        .await;
        assert_eq!(reset.status, 421, "Host {host}");
        let state = http(
            run.control,
            &host,
            Some(&run.token),
            "GET",
            "/control/state",
            None,
        )
        .await;
        assert_eq!(state.status, 421, "Host {host}");
    }
    // Neither reset took effect, and both spellings of the loopback name still work.
    assert_eq!(run.state().await["scenario"], "default");
    let localhost = http(
        run.control,
        &format!("localhost:{own}"),
        Some(&run.token),
        "GET",
        "/control/state",
        None,
    )
    .await;
    assert_eq!(localhost.status, 200);
}

#[tokio::test]
async fn requests_made_through_the_real_routes_change_what_the_fixture_holds() {
    let run = start("default").await;
    let pending = run.get("/api/pending").await.json();
    let id = pending["requests"][0]["id"].as_i64().unwrap();
    let approve = run
        .api(
            "POST",
            &format!("/api/pending/{id}/approve"),
            Some(&json!({})),
        )
        .await;
    assert_eq!(approve.status, 200, "{}", approve.body);
    assert_eq!(run.state().await["pending"], 0);
    assert_eq!(
        run.get("/api/rules").await.json()["rules"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn advancing_the_clock_expires_rules_like_the_sweeper() {
    let run = start("lived-in").await;
    let rules = |reply: Value| reply["rules"].as_array().unwrap().len();
    let before = rules(run.get("/api/rules").await.json());
    let now = run.state().await["now_ms"].as_u64().unwrap();
    let advance = run
        .control("POST", "/control/advance", Some(&json!({"ms": 18_000_000})))
        .await;
    assert_eq!(advance.status, 204);
    assert_eq!(
        run.state().await["now_ms"].as_u64().unwrap(),
        now + 18_000_000
    );
    // `api.example.com` (4 h from 1 h ago) has expired and been swept.
    assert_eq!(rules(run.get("/api/rules").await.json()), before - 1);
}
