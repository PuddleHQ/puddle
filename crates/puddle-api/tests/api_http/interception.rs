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

    let only_switch = api
        .send(
            "PUT",
            "/api/workspaces/shop/interception",
            Some(&json!({"decrypt_all": false})),
        )
        .await;
    let body: Value = only_switch.json();
    assert_eq!(body["decrypt_all"], false);
    assert_eq!(
        body["never_decrypt"],
        json!([]),
        "the names stay as they are"
    );
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

fn shop() -> puddle_types::WorkspaceName {
    puddle_types::WorkspaceName::new("shop").unwrap()
}

#[tokio::test]
async fn the_learned_hosts_are_listed_and_decrypted_again_one_at_a_time_or_all() {
    let api = start().await;
    workspace(&api, "shop").await;
    let none = api
        .send("GET", "/api/workspaces/shop/interception/learned", None)
        .await;
    assert_eq!(none.status, 200, "{}", none.body);
    assert_eq!(none.json(), json!({"hosts": []}));

    api.learned.learn(&shop(), "pinned.example", 600);
    api.learned.learn(&shop(), "other.example", 60);
    let two = api
        .get("/api/workspaces/shop/interception/learned")
        .await
        .json();
    assert_eq!(
        two,
        json!({"hosts": [
            {"host": "other.example", "expires_in_secs": 60},
            {"host": "pinned.example", "expires_in_secs": 600},
        ]}),
        "soonest to expire first"
    );

    let gone = api
        .send(
            "DELETE",
            "/api/workspaces/shop/interception/learned/Pinned.Example.",
            None,
        )
        .await;
    assert_eq!(gone.status, 204, "{}", gone.body);
    let again = api
        .send(
            "DELETE",
            "/api/workspaces/shop/interception/learned/pinned.example",
            None,
        )
        .await;
    assert_eq!(again.status, 404, "{}", again.body);
    let ip = api
        .send(
            "DELETE",
            "/api/workspaces/shop/interception/learned/10.0.0.1",
            None,
        )
        .await;
    assert_eq!(ip.status, 404, "an IP address is never learned");
    let left = api
        .get("/api/workspaces/shop/interception/learned")
        .await
        .json();
    assert_eq!(left["hosts"].as_array().unwrap().len(), 1, "{left}");

    let cleared = api
        .send("DELETE", "/api/workspaces/shop/interception/learned", None)
        .await;
    assert_eq!(cleared.status, 204, "{}", cleared.body);
    let empty = api
        .get("/api/workspaces/shop/interception/learned")
        .await
        .json();
    assert_eq!(empty, json!({"hosts": []}));
}

#[tokio::test]
async fn the_learned_routes_refuse_a_workspace_that_does_not_exist() {
    let api = start().await;
    for (method, path) in [
        ("GET", "/api/workspaces/nope/interception/learned"),
        ("DELETE", "/api/workspaces/nope/interception/learned"),
        (
            "DELETE",
            "/api/workspaces/nope/interception/learned/a.example",
        ),
    ] {
        let reply = api.send(method, path, None).await;
        assert_eq!(reply.status, 404, "{method} {path}: {}", reply.body);
    }
}

#[tokio::test]
async fn the_learned_limits_default_to_twelve_hours_and_1024_hosts_and_refuse_values_out_of_range()
{
    let api = start().await;
    let default = api.get("/api/interception/learned-limits").await;
    assert_eq!(default.status, 200, "{}", default.body);
    assert_eq!(default.json(), json!({"ttl_secs": 43_200, "cap": 1024}));

    let put = api
        .send(
            "PUT",
            "/api/interception/learned-limits",
            Some(&json!({"ttl_secs": 3600, "cap": 64})),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    assert_eq!(put.json(), json!({"ttl_secs": 3600, "cap": 64}));
    assert_eq!(
        api.get("/api/interception/learned-limits").await.json(),
        put.json()
    );

    for bad in [
        json!({"ttl_secs": 59, "cap": 64}),
        json!({"ttl_secs": 2_592_001, "cap": 64}),
        json!({"ttl_secs": 3600, "cap": 0}),
        json!({"ttl_secs": 3600, "cap": 10_001}),
    ] {
        let refused = api
            .send("PUT", "/api/interception/learned-limits", Some(&bad))
            .await;
        assert_eq!(refused.status, 422, "{bad}: {}", refused.body);
    }
    assert_eq!(
        api.get("/api/interception/learned-limits").await.json(),
        json!({"ttl_secs": 3600, "cap": 64}),
        "a refusal changes nothing"
    );
}
