// SPDX-License-Identifier: AGPL-3.0-or-later
//! Clients report their presence and the server says, once per new request, who is told.

use serde_json::{Value, json};

use crate::common::{Api, start};
use crate::events::Stream;

fn report(client: &str, kind: &str, idle_ms: u64, notify: bool) -> Value {
    json!({"client": client, "kind": kind, "visible": true, "idle_ms": idle_ms, "notify": notify})
}

async fn post(api: &Api, body: &Value) -> u16 {
    api.send("POST", "/api/presence", Some(body)).await.status
}

/// The `pending_notify` event after a new request of `workspace` to `host`.
async fn notified(api: &Api, s: &mut Stream, workspace: &str, host: &str) -> Value {
    let before = s
        .data()
        .iter()
        .filter(|d| d["type"] == "pending_notify")
        .count();
    api.request(workspace, host);
    s.read_until(|b| b.matches("\"pending_notify\"").count() > before)
        .await;
    s.data()
        .into_iter()
        .rfind(|d| d["type"] == "pending_notify")
        .unwrap()
}

#[tokio::test]
async fn a_report_is_noted_and_a_malformed_one_is_refused() {
    let api = start().await;
    assert_eq!(post(&api, &report("tab-1", "browser", 0, true)).await, 204);
    for bad in [
        report("", "browser", 0, true),
        report("has space", "browser", 0, true),
        report(&"x".repeat(65), "browser", 0, true),
    ] {
        let reply = api.send("POST", "/api/presence", Some(&bad)).await;
        assert_eq!((reply.status, reply.error()), (400, "bad_request".into()));
    }
    // A kind, a field or a type that is not part of the report is refused.
    assert_eq!(post(&api, &report("tab-1", "toaster", 0, true)).await, 422);
    let mut extra = report("tab-1", "browser", 0, true);
    extra["name"] = json!("x");
    assert_eq!(post(&api, &extra).await, 422);
    api.running.shutdown().await;
}

#[tokio::test]
async fn with_no_client_reporting_the_desktop_app_is_told() {
    let api = start().await;
    let mut s = Stream::open(&api, "").await;
    let event = notified(&api, &mut s, "box", "www.example.com").await;
    assert_eq!(event["clients"], json!([]));
    assert_eq!(event["desktop"], true);
    assert_eq!(event["request"]["host"], "www.example.com");
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_new_request_names_the_client_in_use_and_not_the_others() {
    let api = start().await;
    let mut s = Stream::open(&api, "").await;
    assert_eq!(
        post(&api, &report("desk", "browser", 90_000, true)).await,
        204
    );
    assert_eq!(
        post(&api, &report("phone", "browser", 4_000, true)).await,
        204
    );
    assert_eq!(
        post(&api, &report("app", "desktop", 100_000, true)).await,
        204
    );
    let event = notified(&api, &mut s, "box", "one.example.com").await;
    assert_eq!(event["clients"], json!(["phone"]));
    assert_eq!(event["desktop"], false);

    // Nobody used for over two minutes: every client with its switch on.
    assert_eq!(
        post(&api, &report("desk", "browser", 200_000, true)).await,
        204
    );
    assert_eq!(
        post(&api, &report("phone", "browser", 300_000, false)).await,
        204
    );
    assert_eq!(
        post(&api, &report("app", "desktop", 200_000, true)).await,
        204
    );
    let event = notified(&api, &mut s, "box", "two.example.com").await;
    assert_eq!(event["clients"], json!(["desk"]));
    assert_eq!(event["desktop"], true);
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_event_follows_the_opened_one_and_is_said_once_per_request() {
    let api = start().await;
    let mut s = Stream::open(&api, "").await;
    assert_eq!(post(&api, &report("tab", "browser", 0, true)).await, 204);
    let id = api.request("box", "www.example.com");
    s.read_until(|b| b.contains("pending_notify")).await;
    // The same request seen again is an update, not a new request.
    api.request("box", "www.example.com");
    s.read_until(|b| b.contains("pending_updated")).await;
    let kinds: Vec<_> = s
        .data()
        .iter()
        .map(|d| d["type"].as_str().unwrap().to_owned())
        .filter(|t| t.starts_with("pending"))
        .collect();
    assert_eq!(
        kinds,
        ["pending_opened", "pending_notify", "pending_updated"]
    );
    let event = s.data().into_iter().find(|d| d["type"] == "pending_notify");
    assert_eq!(event.unwrap()["request"]["id"], id);
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_stream_does_not_count_the_servers_own_listener() {
    let api = start().await;
    assert_eq!(api.events.subscribers(), 0);
    let _stream = Stream::open(&api, "").await;
    assert_eq!(api.events.subscribers(), 1);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_workspace_nobody_owns_announces_nothing_and_the_next_request_still_does() {
    let api = start().await;
    let mut s = Stream::open(&api, "").await;
    let summary = |id: i64, workspace: &str| puddle_types::PendingSummary {
        id,
        workspace: puddle_types::WorkspaceId::new(workspace).unwrap(),
        host: "a.example.com".into(),
        registrable_domain: "example.com".into(),
        port: 443,
        first_seen: 1,
        last_seen: 1,
        attempts: 1,
    };
    // A request of a workspace with no recorded user reaches no stream, and nobody is told.
    puddle_types::EventSink::emit(
        &*api.events,
        puddle_types::Event::PendingOpened {
            request: summary(900, "ghost"),
        },
    );
    let event = notified(&api, &mut s, "box", "www.example.com").await;
    assert_ne!(event["request"]["workspace"], "ghost");
    let told: Vec<_> = s
        .data()
        .into_iter()
        .filter(|d| d["type"] == "pending_notify")
        .collect();
    assert_eq!(told.len(), 1, "{told:?}");
    api.running.shutdown().await;
}
