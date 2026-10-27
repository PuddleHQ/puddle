// SPDX-License-Identifier: GPL-3.0-or-later
//! Decrypting every host over real HTTP, against a real store.

use serde_json::{Value, json};

use crate::common::{Api, start};

async fn workspace(api: &Api, name: &str) {
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": name, "repositories": [{"url": "https://github.com/acme/api.git"}]})),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    api.workspaces.idle().await;
}

#[tokio::test]
async fn a_workspace_decrypts_only_its_credential_hosts_until_the_switch_is_turned_on() {
    let api = start().await;
    workspace(&api, "shop").await;
    let off = api
        .send("GET", "/api/workspaces/shop/interception", None)
        .await;
    assert_eq!(off.status, 200, "{}", off.body);
    assert_eq!(
        off.json(),
        json!({"workspace": "shop", "decrypt_all": false, "never_decrypt": [], "global_never_decrypt": []})
    );

    let on = api
        .send(
            "PUT",
            "/api/workspaces/shop/interception",
            Some(&json!({"decrypt_all": true, "never_decrypt": ["Bank.Example.", "*.health.example", "bank.example"]})),
        )
        .await;
    assert_eq!(on.status, 200, "{}", on.body);
    let body: Value = on.json();
    assert_eq!(body["decrypt_all"], true);
    assert_eq!(
        body["never_decrypt"],
        json!(["*.health.example", "bank.example"]),
        "normalised, sorted, without duplicates"
    );

    let only_names = api
        .send(
            "PUT",
            "/api/workspaces/shop/interception",
            Some(&json!({"never_decrypt": []})),
        )
        .await;
    let body: Value = only_names.json();
    assert_eq!(body["decrypt_all"], true, "an absent field stays as it is");
    assert_eq!(body["never_decrypt"], json!([]));
}

#[tokio::test]
async fn a_name_that_cannot_be_decrypted_anyway_is_refused_and_nothing_changes() {
    let api = start().await;
    workspace(&api, "shop").await;
    for bad in ["10.0.0.1", "*.com", "not a host", ""] {
        let reply = api
            .send(
                "PUT",
                "/api/workspaces/shop/interception",
                Some(&json!({"decrypt_all": true, "never_decrypt": ["ok.example", bad]})),
            )
            .await;
        assert_eq!(reply.status, 422, "{bad:?}: {}", reply.body);
    }
    let after = api
        .send("GET", "/api/workspaces/shop/interception", None)
        .await;
    assert_eq!(
        after.json(),
        json!({"workspace": "shop", "decrypt_all": false, "never_decrypt": [], "global_never_decrypt": []})
    );
    let missing = api
        .send("GET", "/api/workspaces/nope/interception", None)
        .await;
    assert_eq!(missing.status, 404);
}

#[tokio::test]
async fn the_global_names_are_shown_in_every_workspace() {
    let api = start().await;
    workspace(&api, "shop").await;
    let put = api
        .send(
            "PUT",
            "/api/interception/never-decrypt",
            Some(&json!({"never_decrypt": ["b.example", "a.example"]})),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    assert_eq!(
        put.json(),
        json!({"never_decrypt": ["a.example", "b.example"]})
    );
    let get = api
        .send("GET", "/api/interception/never-decrypt", None)
        .await;
    assert_eq!(get.json(), put.json());
    let ws = api
        .send("GET", "/api/workspaces/shop/interception", None)
        .await;
    assert_eq!(
        ws.json()["global_never_decrypt"],
        json!(["a.example", "b.example"])
    );
    let too_many: Vec<String> = (0..501).map(|n| format!("h{n}.example")).collect();
    let refused = api
        .send(
            "PUT",
            "/api/interception/never-decrypt",
            Some(&json!({ "never_decrypt": too_many })),
        )
        .await;
    assert_eq!(refused.status, 422);
}
