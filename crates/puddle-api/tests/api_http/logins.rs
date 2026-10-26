// SPDX-License-Identifier: GPL-3.0-or-later
//! Forgetting a kept login over real HTTP.

use serde_json::json;

use crate::common::start;

const REPO: &str = "https://github.com/acme/api.git";

#[tokio::test]
async fn a_kept_login_is_forgotten_by_service_and_unknown_things_are_not_found() {
    let api = start().await;
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "shop", "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    api.workspaces.idle().await;
    let id = "shop".to_owned();

    let gone = api
        .send(
            "DELETE",
            &format!("/api/workspaces/{id}/logins/claude"),
            None,
        )
        .await;
    assert_eq!(gone.status, 204, "{}", gone.body);
    assert_eq!(
        api.workspaces.forgotten_logins(),
        [(id.clone(), "claude".to_owned())]
    );

    let no_service = api
        .send(
            "DELETE",
            &format!("/api/workspaces/{id}/logins/azure"),
            None,
        )
        .await;
    assert_eq!(no_service.status, 404, "{}", no_service.body);
    let no_workspace = api
        .send("DELETE", "/api/workspaces/not-there/logins/claude", None)
        .await;
    assert_eq!(no_workspace.status, 404, "{}", no_workspace.body);

    api.workspaces.set_login_store_refuses(true);
    let refused = api
        .send(
            "DELETE",
            &format!("/api/workspaces/{id}/logins/github"),
            None,
        )
        .await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert_eq!(api.workspaces.forgotten_logins().len(), 1);
}

#[tokio::test]
async fn the_kept_logins_are_listed_without_a_value_and_forgetting_one_takes_it_off_the_list() {
    use puddle_api::{KeptLogin, KeptLogins};
    let api = start().await;
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "shop", "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    api.workspaces.idle().await;
    let path = "/api/workspaces/shop/logins";

    let none = api.send("GET", path, None).await;
    assert_eq!(none.status, 200, "{}", none.body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&none.body).unwrap(),
        json!({"logins": [], "store_readable": true})
    );

    let id = puddle_types::WorkspaceId::new("shop").unwrap();
    api.workspaces.set_kept_logins(
        &id,
        KeptLogins {
            logins: vec![
                KeptLogin {
                    service: "claude".into(),
                    name: "Claude Code".into(),
                    since: Some(1_761_480_000_000),
                },
                KeptLogin {
                    service: "github".into(),
                    name: "GitHub".into(),
                    since: None,
                },
            ],
            store_readable: true,
        },
    );
    let listed = api.send("GET", path, None).await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&listed.body).unwrap(),
        json!({"logins": [
            {"service": "claude", "name": "Claude Code", "since": 1_761_480_000_000_u64},
            {"service": "github", "name": "GitHub", "since": null}
        ], "store_readable": true})
    );

    let gone = api.send("DELETE", &format!("{path}/claude"), None).await;
    assert_eq!(gone.status, 204, "{}", gone.body);
    let after = api.send("GET", path, None).await;
    let after: serde_json::Value = serde_json::from_str(&after.body).unwrap();
    assert_eq!(after["logins"].as_array().unwrap().len(), 1);
    assert_eq!(after["logins"][0]["service"], "github");

    let unknown = api
        .send("GET", "/api/workspaces/not-there/logins", None)
        .await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);
}
