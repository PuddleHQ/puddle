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
