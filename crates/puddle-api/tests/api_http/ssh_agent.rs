// SPDX-License-Identifier: GPL-3.0-or-later
//! The filtered SSH agent's switch over real HTTP, against a real store.

use serde_json::{Value, json};

use crate::common::{Api, start};
use crate::interception::workspace;

const KEY_A: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const KEY_B: &str = "SHA256:Bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb+/9";

async fn hosts(api: &Api, workspace: &str) -> Value {
    let reply = api
        .send(
            "GET",
            &format!("/api/workspaces/{workspace}/ssh-agent"),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()
}

#[tokio::test]
async fn the_agent_is_off_for_every_host_until_the_user_turns_it_on_for_one() {
    let api = start().await;
    workspace(&api, "shop").await;
    assert_eq!(
        hosts(&api, "shop").await,
        json!({"workspace": "shop", "hosts": []})
    );

    let on = api
        .send(
            "PUT",
            "/api/workspaces/shop/ssh-agent/Build.Example.COM.",
            Some(&json!({})),
        )
        .await;
    assert_eq!(on.status, 200, "{}", on.body);
    let body: Value = on.json();
    assert_eq!(body["host"], "build.example.com", "normalised");
    assert_eq!(body["keys"], Value::Null, "every key");

    let limited = api
        .send(
            "PUT",
            "/api/workspaces/shop/ssh-agent/10.0.0.7",
            Some(&json!({"keys": [KEY_B, KEY_A, KEY_B]})),
        )
        .await;
    assert_eq!(limited.status, 200, "{}", limited.body);
    let limited: Value = limited.json();
    assert_eq!(limited["keys"], json!([KEY_A, KEY_B]), "sorted, once each");

    let listed = hosts(&api, "shop").await;
    let names: Vec<&str> = listed["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["host"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["10.0.0.7", "build.example.com"]);

    let off = api
        .send(
            "DELETE",
            "/api/workspaces/shop/ssh-agent/build.example.com",
            None,
        )
        .await;
    assert_eq!(off.status, 204, "{}", off.body);
    let again = api
        .send(
            "DELETE",
            "/api/workspaces/shop/ssh-agent/build.example.com",
            None,
        )
        .await;
    assert_eq!(again.status, 404, "{}", again.body);
    assert_eq!(
        hosts(&api, "shop").await["hosts"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn a_pattern_a_bad_host_or_a_bad_key_list_is_refused_and_nothing_is_saved() {
    let api = start().await;
    workspace(&api, "shop").await;
    for (host, body, says) in [
        ("*.example.com", json!({}), "is a pattern"),
        ("not a host", json!({}), "is not a host name"),
        ("h.example", json!({"keys": []}), "no keys given"),
        (
            "h.example",
            json!({"keys": ["MD5:aa:bb"]}),
            "is not a SHA-256 key fingerprint",
        ),
        (
            "h.example",
            json!({"keys": ["SHA256:short"]}),
            "is not a SHA-256 key fingerprint",
        ),
        (
            "h.example",
            json!({"keys": [format!("{KEY_A}=")]}),
            "is not a SHA-256 key fingerprint",
        ),
        (
            "h.example",
            json!({"keys": vec![KEY_A; 65]}),
            "at most 64 keys",
        ),
        (
            "h.example",
            json!({"keys": null, "comment": "x"}),
            "unknown field",
        ),
    ] {
        let path = format!(
            "/api/workspaces/shop/ssh-agent/{}",
            host.replace(' ', "%20")
        );
        let reply = api.send("PUT", &path, Some(&body)).await;
        assert_eq!(reply.status, 422, "{host} {body}: {}", reply.body);
        assert!(reply.body.contains(says), "{host} {body}: {}", reply.body);
    }
    assert_eq!(hosts(&api, "shop").await["hosts"], json!([]));
}

#[tokio::test]
async fn an_unknown_workspace_is_not_found_and_a_workspace_has_a_limit_of_hosts() {
    let api = start().await;
    for (method, path) in [
        ("GET", "/api/workspaces/nobody/ssh-agent"),
        ("PUT", "/api/workspaces/nobody/ssh-agent/h.example"),
        ("DELETE", "/api/workspaces/nobody/ssh-agent/h.example"),
    ] {
        let reply = api.send(method, path, Some(&json!({}))).await;
        assert_eq!(reply.status, 404, "{method} {path}: {}", reply.body);
    }
    workspace(&api, "shop").await;
    for n in 0..100 {
        let reply = api
            .send(
                "PUT",
                &format!("/api/workspaces/shop/ssh-agent/h{n}.example"),
                Some(&json!({})),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    }
    let over = api
        .send(
            "PUT",
            "/api/workspaces/shop/ssh-agent/one-more.example",
            Some(&json!({})),
        )
        .await;
    assert_eq!(over.status, 422, "{}", over.body);
    assert!(over.body.contains("at most 100 hosts"), "{}", over.body);
}
