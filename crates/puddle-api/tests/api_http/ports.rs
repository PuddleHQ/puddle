// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's ports over real HTTP: the list, a forward, a pin, and every refusal.

use puddle_api::{ApiConfig, ApiServer, ApiToken, EventHub, MemorySettings, Services};
use puddle_store::{Limits, ManualClock, Store};
use puddle_types::WorkspaceName;
use serde_json::json;
use std::sync::Arc;

use crate::common::{Api, START_MS, raw, start};

const REPO: &str = "https://github.com/acme/api.git";

async fn workspace(api: &Api, name: &str) {
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": name, "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    api.workspaces.idle().await;
}

fn shop() -> WorkspaceName {
    WorkspaceName::new("shop").unwrap()
}

#[tokio::test]
async fn the_list_shows_what_listens_and_a_forward_pins_and_stops() {
    let api = start().await;
    workspace(&api, "shop").await;
    api.ports.set_listening(&shop(), &[8080, 3000]);

    let list = api.get("/api/workspaces/shop/ports").await;
    assert_eq!(list.status, 200, "{}", list.body);
    let list = list.json();
    assert_eq!(list["running"], true);
    assert_eq!(
        list["ports"],
        json!([
            {"guest_port": 3000, "listening": true, "forwarded": false, "pinned": false, "host_port": null, "problem": null},
            {"guest_port": 8080, "listening": true, "forwarded": false, "pinned": false, "host_port": null, "problem": null},
        ])
    );

    // Forward 3000 to the same number; then pin 8080 to another.
    let first = api
        .send("PUT", "/api/workspaces/shop/ports/3000", Some(&json!({})))
        .await;
    assert_eq!(first.status, 200, "{}", first.body);
    assert_eq!(
        first.json(),
        json!({"guest_port": 3000, "listening": true, "forwarded": true, "pinned": false, "host_port": 3000, "problem": null})
    );
    let pinned = api
        .send(
            "PUT",
            "/api/workspaces/shop/ports/8080",
            Some(&json!({"host_port": 18080, "pinned": true})),
        )
        .await
        .json();
    assert_eq!(
        (pinned["host_port"].clone(), pinned["pinned"].clone()),
        (json!(18080), json!(true))
    );

    // A port of this computer another forward has is a conflict that names it.
    let clash = api
        .send(
            "PUT",
            "/api/workspaces/shop/ports/9000",
            Some(&json!({"host_port": 18080})),
        )
        .await;
    assert_eq!(clash.status, 409, "{}", clash.body);
    assert_eq!(clash.error(), "conflict");
    assert!(
        clash.json()["message"]
            .as_str()
            .unwrap()
            .contains("already in use by")
    );

    // Stopping removes the forward and the pin; a port that listens stays listed.
    let gone = api
        .send("DELETE", "/api/workspaces/shop/ports/8080", None)
        .await;
    assert_eq!(gone.status, 204, "{}", gone.body);
    let list = api.get("/api/workspaces/shop/ports").await.json();
    assert_eq!(list["ports"][1]["forwarded"], false);
    assert_eq!(list["ports"][1]["listening"], true);
    let again = api
        .send("DELETE", "/api/workspaces/shop/ports/8080", None)
        .await;
    assert_eq!(again.status, 404, "{}", again.body);
    assert_eq!(again.error(), "not_found");

    // The guest's report replaces the listening flags: 3000 stays (forwarded), 8080 is no longer
    // listed once nothing listens there; a forward of a port that never listened vanishes with its stop.
    api.ports.set_listening(&shop(), &[3000]);
    let list = api.get("/api/workspaces/shop/ports").await.json();
    assert_eq!(list["ports"].as_array().unwrap().len(), 1, "{list}");
    let ahead = api
        .send("PUT", "/api/workspaces/shop/ports/5000", Some(&json!({})))
        .await;
    assert_eq!(ahead.status, 200, "{}", ahead.body);
    let stopped = api
        .send("DELETE", "/api/workspaces/shop/ports/5000", None)
        .await;
    assert_eq!(stopped.status, 204, "{}", stopped.body);
    let list = api.get("/api/workspaces/shop/ports").await.json();
    assert_eq!(list["ports"].as_array().unwrap().len(), 1, "{list}");
}

#[tokio::test]
async fn a_port_that_is_not_a_port_a_missing_workspace_and_a_bad_body_are_refused() {
    let api = start().await;
    workspace(&api, "shop").await;
    let zero = api
        .send(
            "PUT",
            "/api/workspaces/shop/ports/3000",
            Some(&json!({"host_port": 0})),
        )
        .await;
    assert_eq!(zero.status, 422, "{}", zero.body);
    assert_eq!(zero.error(), "invalid");
    for path in [
        "/api/workspaces/shop/ports/0",
        "/api/workspaces/shop/ports/70000",
        "/api/workspaces/shop/ports/x",
    ] {
        let bad = api.send("PUT", path, Some(&json!({}))).await;
        assert!(
            bad.status == 400 || bad.status == 422,
            "{path}: {} {}",
            bad.status,
            bad.body
        );
    }
    let unknown_field = api
        .send(
            "PUT",
            "/api/workspaces/shop/ports/3000",
            Some(&json!({"pin": true})),
        )
        .await;
    assert!(
        unknown_field.status >= 400 && unknown_field.status < 500,
        "{}",
        unknown_field.body
    );
    let missing = api.get("/api/workspaces/nothere/ports").await;
    assert_eq!(missing.status, 404, "{}", missing.body);
    let missing = api
        .send("PUT", "/api/workspaces/nothere/ports/80", Some(&json!({})))
        .await;
    assert_eq!(missing.status, 404, "{}", missing.body);
    let missing = api
        .send("DELETE", "/api/workspaces/nothere/ports/80", None)
        .await;
    assert_eq!(missing.status, 404, "{}", missing.body);
}

#[tokio::test]
async fn without_a_ports_service_every_call_says_it_is_unavailable() {
    // `Services::new` starts with none wired; the workspaces service is the fake one.
    let clock = Arc::new(ManualClock::new(START_MS));
    let events = Arc::new(EventHub::default());
    let store = Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap());
    let workspaces = puddle_api::FakeWorkspaces::with_options(
        events.clone(),
        clock.clone(),
        Arc::new(puddle_api::FakeLauncher::new()),
        std::time::Duration::ZERO,
    );
    let services = Services::new(store, Arc::new(MemorySettings::default()), events, clock)
        .with_workspaces(Arc::new(workspaces.clone()));
    let token = ApiToken::generate().unwrap();
    let bearer = token.expose().to_owned();
    let server = ApiServer::bind(ApiConfig::default(), token, services)
        .await
        .unwrap();
    let addr = server.local_addr();
    let _running = server.spawn();
    let send = |method: &str, path: &str, body: &str| {
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {bearer}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            addr.port(),
            body.len()
        );
        async move { raw(addr, head.as_bytes()).await }
    };
    let made = send(
        "POST",
        "/api/workspaces",
        &json!({"name": "shop", "repositories": [{"url": REPO}]}).to_string(),
    )
    .await;
    assert_eq!(made.status, 202, "{}", made.body);
    workspaces.idle().await;
    for (method, path, body) in [
        ("GET", "/api/workspaces/shop/ports", ""),
        ("PUT", "/api/workspaces/shop/ports/80", "{}"),
        ("DELETE", "/api/workspaces/shop/ports/80", ""),
    ] {
        let reply = send(method, path, body).await;
        assert_eq!(reply.status, 503, "{method} {path}: {}", reply.body);
        assert_eq!(reply.error(), "unavailable");
    }
}
