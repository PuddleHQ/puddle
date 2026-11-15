// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decrypting every host over real HTTP, against a real store.

use serde_json::{Value, json};

use crate::common::{Api, start};

pub(crate) async fn workspace(api: &Api, name: &str) {
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
        json!({"workspace": "shop", "mode": "inherit", "effective_mode": "known_only", "never_decrypt": [], "global_never_decrypt": []})
    );

    let on = api
        .send(
            "PUT",
            "/api/workspaces/shop/interception",
            Some(&json!({"mode": "all", "never_decrypt": ["Bank.Example.", "*.health.example", "bank.example"]})),
        )
        .await;
    assert_eq!(on.status, 200, "{}", on.body);
    let body: Value = on.json();
    assert_eq!(body["mode"], "all");
    assert_eq!(body["effective_mode"], "all");
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
    assert_eq!(body["mode"], "all", "an absent field stays as it is");
    assert_eq!(body["never_decrypt"], json!([]));

    let only_switch = api
        .send(
            "PUT",
            "/api/workspaces/shop/interception",
            Some(&json!({"mode": "inherit"})),
        )
        .await;
    let body: Value = only_switch.json();
    assert_eq!(body["mode"], "inherit");
    assert_eq!(body["effective_mode"], "known_only");
    assert_eq!(
        body["never_decrypt"],
        json!([]),
        "the names stay as they are"
    );
}

#[tokio::test]
async fn the_global_mode_is_unchosen_known_only_until_set_and_a_workspace_follows_or_overrides_it()
{
    let api = start().await;
    workspace(&api, "shop").await;
    let fresh = api.send("GET", "/api/interception", None).await;
    assert_eq!(fresh.status, 200, "{}", fresh.body);
    let body: Value = fresh.json();
    assert_eq!(body["mode"], "known_only");
    assert_eq!(body["chosen"], false);
    assert_eq!(body["learn_from_rejections"], true);
    assert_eq!(body["notice_seen"], false);

    let put = api
        .send(
            "PUT",
            "/api/interception",
            Some(&json!({"mode": "all_except_curated"})),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    let body: Value = put.json();
    assert_eq!(body["mode"], "all_except_curated");
    assert_eq!(body["chosen"], true);
    assert_eq!(body["notice_seen"], true, "choosing answers the notice");

    let follows = api
        .send("GET", "/api/workspaces/shop/interception", None)
        .await
        .json();
    assert_eq!(follows["mode"], "inherit");
    assert_eq!(follows["effective_mode"], "all_except_curated");

    let own = api
        .send(
            "PUT",
            "/api/workspaces/shop/interception",
            Some(&json!({"mode": "known_only"})),
        )
        .await
        .json();
    assert_eq!(own["mode"], "known_only");
    assert_eq!(own["effective_mode"], "known_only", "its own mode wins");

    let curated = api
        .send(
            "PUT",
            "/api/workspaces/shop/interception",
            Some(&json!({"mode": "all_except_curated"})),
        )
        .await
        .json();
    assert_eq!(curated["mode"], "all_except_curated");
    assert_eq!(curated["effective_mode"], "all_except_curated");
    let back = api
        .send("GET", "/api/workspaces/shop/interception", None)
        .await
        .json();
    assert_eq!(
        back["mode"], "all_except_curated",
        "the choice is read back"
    );

    let seen = api
        .send(
            "PUT",
            "/api/interception",
            Some(&json!({"notice_seen": false})),
        )
        .await
        .json();
    assert_eq!(
        seen["notice_seen"], false,
        "the notice flag is its own field"
    );
    assert_eq!(
        seen["mode"], "all_except_curated",
        "and leaves the mode alone"
    );

    let off = api
        .send(
            "PUT",
            "/api/interception",
            Some(&json!({"learn_from_rejections": false})),
        )
        .await
        .json();
    assert_eq!(off["learn_from_rejections"], false);
    assert_eq!(off["mode"], "all_except_curated", "an absent field stays");

    let bad = api
        .send("PUT", "/api/interception", Some(&json!({"mode": "most"})))
        .await;
    assert_eq!(bad.status, 422, "{}", bad.body);
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
                Some(&json!({"mode": "all", "never_decrypt": ["ok.example", bad]})),
            )
            .await;
        assert_eq!(reply.status, 422, "{bad:?}: {}", reply.body);
    }
    let after = api
        .send("GET", "/api/workspaces/shop/interception", None)
        .await;
    assert_eq!(
        after.json(),
        json!({"workspace": "shop", "mode": "inherit", "effective_mode": "known_only", "never_decrypt": [], "global_never_decrypt": []})
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

fn shop() -> puddle_types::WorkspaceId {
    puddle_types::WorkspaceId::new("shop").unwrap()
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
            {"host": "other.example", "passed_through": true, "expires_in_secs": 60,
             "first_rejected_at": 1, "last_rejected_at": 1, "count": 1},
            {"host": "pinned.example", "passed_through": true, "expires_in_secs": 600,
             "first_rejected_at": 1, "last_rejected_at": 1, "count": 1},
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
    assert_eq!(
        left["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| (
                h["host"].as_str().unwrap(),
                h["passed_through"].as_bool().unwrap()
            ))
            .collect::<Vec<_>>(),
        [("other.example", true), ("pinned.example", false)],
        "a host decrypted again stays listed with its history: {left}"
    );

    let cleared = api
        .send("DELETE", "/api/workspaces/shop/interception/learned", None)
        .await;
    assert_eq!(cleared.status, 204, "{}", cleared.body);
    let empty = api
        .get("/api/workspaces/shop/interception/learned")
        .await
        .json();
    assert_eq!(
        empty["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["passed_through"].as_bool().unwrap())
            .collect::<Vec<_>>(),
        [false, false],
        "clearing keeps the history: {empty}"
    );
}

#[tokio::test]
async fn a_learned_host_is_made_permanent_for_the_workspace_or_for_every_workspace() {
    let api = start().await;
    workspace(&api, "shop").await;
    api.learned
        .learn_with_history(&shop(), "pinned.example", Some(600), 3, 5_000);
    api.learned
        .learn_with_history(&shop(), "other.example", None, 2, 4_000);
    let listed = api
        .get("/api/workspaces/shop/interception/learned")
        .await
        .json();
    assert_eq!(
        listed["hosts"][0],
        json!({"host": "pinned.example", "passed_through": true, "expires_in_secs": 600,
               "first_rejected_at": 3_000, "last_rejected_at": 5_000, "count": 3})
    );

    let own = api
        .send(
            "POST",
            "/api/workspaces/shop/interception/learned/Pinned.Example./permanent",
            Some(&json!({"scope": "workspace"})),
        )
        .await;
    assert_eq!(own.status, 204, "{}", own.body);
    let view = api.get("/api/workspaces/shop/interception").await.json();
    assert_eq!(view["never_decrypt"], json!(["pinned.example"]));
    assert_eq!(view["global_never_decrypt"], json!([]));
    let left = api
        .get("/api/workspaces/shop/interception/learned")
        .await
        .json();
    assert_eq!(
        left["hosts"].as_array().unwrap().len(),
        1,
        "gone from the list: {left}"
    );

    let global = api
        .send(
            "POST",
            "/api/workspaces/shop/interception/learned/other.example/permanent",
            Some(&json!({"scope": "global"})),
        )
        .await;
    assert_eq!(global.status, 204, "{}", global.body);
    let view = api.get("/api/workspaces/shop/interception").await.json();
    assert_eq!(view["global_never_decrypt"], json!(["other.example"]));
    assert_eq!(
        view["never_decrypt"],
        json!(["pinned.example"]),
        "the workspace's own list is kept"
    );

    for host in ["other.example", "10.0.0.1"] {
        let reply = api
            .send(
                "POST",
                &format!("/api/workspaces/shop/interception/learned/{host}/permanent"),
                Some(&json!({"scope": "global"})),
            )
            .await;
        assert_eq!(reply.status, 404, "{host}: {}", reply.body);
    }
}

#[tokio::test]
async fn a_full_never_decrypt_list_refuses_to_take_a_learned_host_and_keeps_it_listed() {
    let api = start().await;
    workspace(&api, "shop").await;
    let names: Vec<String> = (0..500).map(|n| format!("h{n}.example")).collect();
    let put = api
        .send(
            "PUT",
            "/api/workspaces/shop/interception",
            Some(&json!({"never_decrypt": names})),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    api.learned.learn(&shop(), "pinned.example", 600);
    let full = api
        .send(
            "POST",
            "/api/workspaces/shop/interception/learned/pinned.example/permanent",
            Some(&json!({"scope": "workspace"})),
        )
        .await;
    assert_eq!(full.status, 422, "{}", full.body);
    let listed = api
        .get("/api/workspaces/shop/interception/learned")
        .await
        .json();
    assert_eq!(listed["hosts"].as_array().unwrap().len(), 1);
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
        (
            "POST",
            "/api/workspaces/nope/interception/learned/a.example/permanent",
        ),
    ] {
        let body = json!({"scope": "global"});
        let reply = api
            .send(method, path, (method == "POST").then_some(&body))
            .await;
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
