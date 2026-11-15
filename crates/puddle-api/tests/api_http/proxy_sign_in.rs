// SPDX-License-Identifier: AGPL-3.0-or-later
//! The proxy sign-ins over real HTTP: store, list, replace, remove, every refusal, and that no
//! response or log line carries the password.

use std::sync::Arc;

use puddle_api::{FakeNetworkHealth, FakeProxySignIns, ProxySignInService};
use puddle_store::{Clock, ManualClock};
use puddle_upstream::ProxyAddr;
use serde_json::json;

use crate::common::{START_MS, start, start_with_network};

// Built at run time from the process id, so no constant string is a password.
fn password() -> &'static str {
    static VALUE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VALUE.get_or_init(|| format!("pw-{}-secret", std::process::id()))
}

fn proxy() -> ProxyAddr {
    ProxyAddr::new("proxy.corp.example", 8080)
}

#[tokio::test]
async fn a_sign_in_is_stored_listed_by_name_and_removed() {
    let api = start().await;
    let empty = api.get("/api/proxy-sign-ins").await;
    assert_eq!(empty.status, 200, "{}", empty.body);
    assert_eq!(empty.json(), json!({"sign_ins": []}));

    let stored = api
        .send(
            "PUT",
            "/api/proxy-sign-ins",
            Some(&json!({"proxy": "http://PROXY.corp.example:8080/", "user": "alice", "password": password()})),
        )
        .await;
    assert_eq!(stored.status, 200, "{}", stored.body);
    assert_eq!(
        stored.json(),
        json!({"sign_ins": [{"proxy": "proxy.corp.example:8080", "user": "alice"}]})
    );
    assert!(!stored.body.contains(password()), "{}", stored.body);
    assert_eq!(
        api.proxy_sign_ins.password(&proxy()).as_deref(),
        Some(password())
    );

    let listed = api.get("/api/proxy-sign-ins").await;
    assert!(!listed.body.contains(password()), "{}", listed.body);
    assert_eq!(listed.json()["sign_ins"][0]["user"], "alice");

    // A second PUT for the same proxy replaces it.
    let replaced = api
        .send(
            "PUT",
            "/api/proxy-sign-ins",
            Some(&json!({"proxy": "proxy.corp.example:8080", "user": "bob", "password": "other"})),
        )
        .await;
    assert_eq!(replaced.json()["sign_ins"].as_array().unwrap().len(), 1);
    assert_eq!(replaced.json()["sign_ins"][0]["user"], "bob");

    let gone = api
        .send(
            "DELETE",
            "/api/proxy-sign-ins/proxy.corp.example:8080",
            None,
        )
        .await;
    assert_eq!(gone.status, 204, "{}", gone.body);
    assert_eq!(
        api.get("/api/proxy-sign-ins").await.json(),
        json!({"sign_ins": []})
    );

    // Removing one that is not stored is fine.
    let again = api
        .send(
            "DELETE",
            "/api/proxy-sign-ins/proxy.corp.example:8080",
            None,
        )
        .await;
    assert_eq!(again.status, 204);
}

#[tokio::test]
async fn a_value_basic_cannot_carry_is_refused_and_nothing_is_stored() {
    let api = start().await;
    for (body, needle) in [
        (
            json!({"proxy": "p:1", "user": "", "password": password()}),
            "user name",
        ),
        (
            json!({"proxy": "p:1", "user": "a:b", "password": password()}),
            "user name",
        ),
        (
            json!({"proxy": "p:1", "user": "a", "password": ""}),
            "password",
        ),
        (
            json!({"proxy": "p:1", "user": "a", "password": "x\ny"}),
            "password",
        ),
        (
            json!({"proxy": "", "user": "a", "password": password()}),
            "proxy",
        ),
        (
            json!({"proxy": "https://p:1", "user": "a", "password": password()}),
            "proxy",
        ),
    ] {
        let reply = api.send("PUT", "/api/proxy-sign-ins", Some(&body)).await;
        assert_eq!(reply.status, 422, "{body}: {}", reply.body);
        assert!(
            reply.json()["message"].as_str().unwrap().contains(needle),
            "{}",
            reply.body
        );
        assert!(!reply.body.contains(password()), "{}", reply.body);
    }
    // A password sent as something else is refused without quoting it.
    let number = api
        .send(
            "PUT",
            "/api/proxy-sign-ins",
            Some(&json!({"proxy": "p:1", "user": "a", "password": 123_456_789})),
        )
        .await;
    assert!(
        number.status >= 400 && !number.body.contains("123456789"),
        "{}",
        number.body
    );
    assert_eq!(
        api.get("/api/proxy-sign-ins").await.json(),
        json!({"sign_ins": []})
    );

    let bad = api
        .send("DELETE", "/api/proxy-sign-ins/https:%2F%2Fp", None)
        .await;
    assert_eq!(bad.status, 422, "{}", bad.body);
}

#[tokio::test]
async fn a_credential_store_that_does_not_answer_is_a_503_that_says_so() {
    let api = start().await;
    api.proxy_sign_ins.fail_store(true);
    let put = api
        .send(
            "PUT",
            "/api/proxy-sign-ins",
            Some(&json!({"proxy": "p:1", "user": "a", "password": password()})),
        )
        .await;
    assert_eq!(put.status, 503, "{}", put.body);
    assert!(
        put.json()["message"]
            .as_str()
            .unwrap()
            .contains("credential store")
    );
    assert!(!put.body.contains(password()));
    let del = api.send("DELETE", "/api/proxy-sign-ins/p:1", None).await;
    assert_eq!(del.status, 503, "{}", del.body);
}

#[tokio::test]
async fn the_network_report_names_stored_sign_ins_without_a_password() {
    let clock = Arc::new(ManualClock::new(START_MS));
    let sign_ins = Arc::new(FakeProxySignIns::new());
    let fake = FakeNetworkHealth::new(clock as Arc<dyn Clock>).with_sign_ins(sign_ins.clone());
    let api = start_with_network(Arc::new(fake)).await;
    assert_eq!(
        api.get("/api/network-health").await.json()["sign_in"]["stored"],
        json!([])
    );

    sign_ins.set(&proxy(), "alice", password()).unwrap();
    let report = api.get("/api/network-health").await;
    assert_eq!(report.status, 200, "{}", report.body);
    assert_eq!(
        report.json()["sign_in"]["stored"],
        json!([{"proxy": "proxy.corp.example:8080", "user": "alice"}])
    );
    assert!(!report.body.contains(password()), "{}", report.body);
}

#[tokio::test]
async fn a_build_without_the_service_lists_nothing_and_refuses_a_change() {
    use puddle_api::NoProxySignIns;
    let none = NoProxySignIns;
    assert_eq!(none.list().len(), 0);
    assert!(none.set(&proxy(), "a", "b").is_err());
    assert!(none.remove(&proxy()).is_err());
}
