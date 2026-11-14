// SPDX-License-Identifier: AGPL-3.0-or-later
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

const FINGERPRINT: &str = "SHA256:Cccccccccccccccccccccccccccccccccccccccc+/9";

/// A guest's login to `host` with the key waits as a question, as the host's gate puts it.
fn ask(api: &Api, workspace: &str, host: &str) -> i64 {
    match api
        .store
        .start_ssh_agent_ask(
            &puddle_types::WorkspaceId::new(workspace).unwrap(),
            host,
            FINGERPRINT,
            "ssh-ed25519",
        )
        .unwrap()
    {
        puddle_store::SshAgentAskStart::Open(id) => id,
        puddle_store::SshAgentAskStart::Granted => panic!("a grant answered"),
    }
}

async fn asks(api: &Api) -> Value {
    let reply = api.send("GET", "/api/ssh-agent-asks", None).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()
}

#[tokio::test]
async fn a_question_the_agent_puts_is_listed_with_who_asks_and_which_key() {
    let api = start().await;
    workspace(&api, "shop").await;
    assert_eq!(asks(&api).await, json!({"asks": []}));
    let id = ask(&api, "shop", "build.example.com");
    let listed = asks(&api).await;
    assert_eq!(
        listed["asks"],
        json!([{
            "id": id,
            "workspace": "shop",
            "host": "build.example.com",
            "key_type": "ssh-ed25519",
            "fingerprint": FINGERPRINT,
            "first_seen": listed["asks"][0]["first_seen"],
            "last_seen": listed["asks"][0]["last_seen"],
            "attempts": 1
        }])
    );
}

#[tokio::test]
async fn an_answer_ends_the_question_and_always_leaves_a_grant_that_stops_later_ones() {
    let api = start().await;
    workspace(&api, "shop").await;
    let id = ask(&api, "shop", "build.example.com");
    let url = format!("/api/workspaces/shop/ssh-agent-asks/{id}/answer");
    let done = api
        .send("POST", &url, Some(&json!({"answer": "always"})))
        .await;
    assert_eq!(done.status, 204, "{}", done.body);
    assert_eq!(asks(&api).await, json!({"asks": []}));
    let granted = api
        .store
        .start_ssh_agent_ask(
            &puddle_types::WorkspaceId::new("shop").unwrap(),
            "build.example.com",
            FINGERPRINT,
            "ssh-ed25519",
        )
        .unwrap();
    assert_eq!(granted, puddle_store::SshAgentAskStart::Granted);
    let again = api
        .send("POST", &url, Some(&json!({"answer": "deny"})))
        .await;
    assert_eq!(again.status, 409, "{}", again.body);
}

#[tokio::test]
async fn an_answer_names_one_of_four_and_a_question_of_another_workspace_is_not_found() {
    let api = start().await;
    workspace(&api, "shop").await;
    workspace(&api, "lab").await;
    let id = ask(&api, "shop", "build.example.com");
    for (path, body, status) in [
        (
            format!("/api/workspaces/shop/ssh-agent-asks/{id}/answer"),
            json!({"answer": "maybe"}),
            422,
        ),
        (
            format!("/api/workspaces/shop/ssh-agent-asks/{id}/answer"),
            json!({}),
            422,
        ),
        (
            format!("/api/workspaces/lab/ssh-agent-asks/{id}/answer"),
            json!({"answer": "once"}),
            404,
        ),
        (
            "/api/workspaces/shop/ssh-agent-asks/999/answer".to_owned(),
            json!({"answer": "once"}),
            404,
        ),
        (
            format!("/api/workspaces/nothere/ssh-agent-asks/{id}/answer"),
            json!({"answer": "once"}),
            404,
        ),
    ] {
        let reply = api.send("POST", &path, Some(&body)).await;
        assert_eq!(reply.status, status, "{path}: {}", reply.body);
    }
    assert_eq!(asks(&api).await["asks"].as_array().unwrap().len(), 1);
    for answer in ["once", "run", "deny"] {
        let id = ask(&api, "lab", &format!("{answer}.example.com"));
        let reply = api
            .send(
                "POST",
                &format!("/api/workspaces/lab/ssh-agent-asks/{id}/answer"),
                Some(&json!({"answer": answer})),
            )
            .await;
        assert_eq!(reply.status, 204, "{answer}: {}", reply.body);
    }
}

#[tokio::test]
async fn turning_the_agent_off_for_a_host_takes_its_questions_off_the_list() {
    let api = start().await;
    workspace(&api, "shop").await;
    let put = api
        .send(
            "PUT",
            "/api/workspaces/shop/ssh-agent/build.example.com",
            Some(&json!({})),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    ask(&api, "shop", "build.example.com");
    let off = api
        .send(
            "DELETE",
            "/api/workspaces/shop/ssh-agent/build.example.com",
            None,
        )
        .await;
    assert_eq!(off.status, 204, "{}", off.body);
    assert_eq!(asks(&api).await, json!({"asks": []}));
}
