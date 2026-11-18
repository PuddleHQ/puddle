// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rules and the switches around them across a full host restart: every decision a guest meets
//! through the sandbox proxy is the same afterwards, and a rule that expired stays gone.

use futures_util::StreamExt as _;
use puddle_agent_proto::tokio_yamux::Session;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

/// What a guest's `CONNECT` meets at the proxy. Without a guest or an upstream, a name a rule
/// allows is either answered `200` (a host Puddle terminates itself) or ends at the resolver,
/// which knows no names (`502 no such name`): both are `Allowed`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Meets {
    Allowed,
    Deny,
    Pending,
    Blocked(String),
}

/// One `CONNECT <authority>` from `workspace`'s guest over the workspace's proxy route.
async fn connect(rig: &Rig, workspace: &str, authority: &str) -> Meets {
    let sandbox = WorkspaceId::new(workspace).unwrap().sandbox_name();
    let routes = rig.runtime.routes(&sandbox);
    let conn = puddle_ipc::connect(&routes.first().unwrap().host)
        .await
        .unwrap();
    let mut session = Session::new_client(conn, puddle_agent_proto::yamux::client_config());
    let mut control = session.control();
    let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
    let mut stream = control.open_stream().await.unwrap();
    let head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(10), async {
        let mut chunk = [0_u8; 1024];
        while !reply.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => reply.extend_from_slice(&chunk[..n]),
            }
        }
    })
    .await;
    driver.abort();
    assert!(
        read.is_ok(),
        "no answer to CONNECT {authority} from {workspace}"
    );
    let text = String::from_utf8_lossy(&reply).into_owned();
    let header = |name: &str| {
        text.lines().find_map(|line| {
            let (n, v) = line.split_once(':')?;
            n.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
        })
    };
    match header("x-puddle-decision").as_deref() {
        Some("deny") => Meets::Deny,
        Some("pending") => Meets::Pending,
        Some("blocked") => Meets::Blocked(header("x-puddle-blocked").unwrap_or_default()),
        _ => {
            assert!(
                text.starts_with("HTTP/1.1 502") || text.starts_with("HTTP/1.1 200"),
                "CONNECT {authority} from {workspace} got neither a decision nor an allow: {text}"
            );
            Meets::Allowed
        }
    }
}

async fn created(api: &Api, path: &str, body: serde_json::Value) -> serde_json::Value {
    let reply = api.post(path, &body.to_string()).await;
    assert_eq!(reply.status, 201, "{path}: {}", reply.body);
    reply.json()
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// Every probe the test repeats, in one list: (what it is, workspace, authority, what it meets).
fn probes() -> Vec<(&'static str, &'static str, &'static str, Meets)> {
    use Meets::{Allowed, Blocked, Pending};
    vec![
        (
            "exact rule, every workspace",
            "acme",
            "exact.test:443",
            Allowed,
        ),
        (
            "exact rule, other workspace",
            "beta",
            "exact.test:443",
            Allowed,
        ),
        (
            "exact rule does not widen",
            "acme",
            "sub.exact.test:443",
            Pending,
        ),
        (
            "suffix rule below",
            "acme",
            "deep.under.suffix.test:443",
            Allowed,
        ),
        (
            "suffix rule, other workspace",
            "beta",
            "deep.under.suffix.test:443",
            Pending,
        ),
        (
            "suffix rule not at the apex",
            "acme",
            "suffix.test:443",
            Pending,
        ),
        (
            "expiring rule still valid",
            "acme",
            "later.test:443",
            Allowed,
        ),
        ("expired rule stays gone", "acme", "soon.test:443", Pending),
        ("own set, on for acme", "acme", "own-set.test:443", Allowed),
        ("own set, off for beta", "beta", "own-set.test:443", Pending),
        (
            "own set's deny, on for acme",
            "acme",
            "own-set-deny.test:443",
            Meets::Deny,
        ),
        (
            "built-in set, on for acme",
            "acme",
            "github.com:443",
            Allowed,
        ),
        (
            "built-in set, off for beta",
            "beta",
            "github.com:443",
            Pending,
        ),
        (
            "loopback toggle on for acme",
            "acme",
            "127.0.0.1:9",
            Pending,
        ),
        (
            "loopback toggle off for beta",
            "beta",
            "127.0.0.1:9",
            Blocked("toggle:loopback".into()),
        ),
    ]
}

async fn assert_decisions(rig: &Rig, when: &str) {
    for (what, workspace, authority, expected) in probes() {
        let got = connect(rig, workspace, authority).await;
        assert_eq!(got, expected, "{when}: {what} ({workspace} -> {authority})");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_rule_set_override_and_toggle_decides_the_same_after_a_full_restart() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    create(&api1, &mut events, "beta").await;

    // An exact rule and a suffix rule for one workspace.
    created(
        &api1,
        "/api/rules",
        json!({"scope": {"type": "global"}, "pattern": "exact.test", "effect": "allow"}),
    )
    .await;
    created(
        &api1,
        "/api/rules",
        json!({"scope": {"type": "workspace", "workspace": "acme"}, "pattern": "*.suffix.test", "effect": "allow"}),
    )
    .await;
    // One rule that is valid for an hour, one that runs out in a moment.
    let hour = now_ms() + 3_600_000;
    created(
        &api1,
        "/api/rules",
        json!({"scope": {"type": "global"}, "pattern": "later.test", "effect": "allow", "expires_at": hour}),
    )
    .await;
    let soon = now_ms() + 2_500;
    created(
        &api1,
        "/api/rules",
        json!({"scope": {"type": "global"}, "pattern": "soon.test", "effect": "allow", "expires_at": soon}),
    )
    .await;
    assert_eq!(
        connect(&rig, "acme", "soon.test:443").await,
        Meets::Allowed,
        "the expiring rule allows before it expires"
    );

    // A set of your own, switched off everywhere and on for acme alone, with an allow and a deny.
    let set = created(&api1, "/api/rule-sets", json!({"name": "Mine"})).await;
    let set_id = set["id"].as_str().unwrap().to_owned();
    let set_number: i64 = set_id.strip_prefix("user:").unwrap().parse().unwrap();
    for (pattern, effect) in [("own-set.test", "allow"), ("own-set-deny.test", "deny")] {
        created(
            &api1,
            "/api/rules",
            json!({"scope": {"type": "set", "set": set_number}, "pattern": pattern, "effect": effect}),
        )
        .await;
    }
    let switch = |workspace: serde_json::Value, enabled: bool| {
        json!({"workspace": workspace, "enabled": enabled}).to_string()
    };
    for (id, body) in [
        (&set_id, switch(json!(null), false)),
        (&set_id, switch(json!("acme"), true)),
        (&"builtin:github".to_owned(), switch(json!("acme"), true)),
    ] {
        let reply = api1
            .put(&format!("/api/rule-sets/{id}/switch"), &body)
            .await;
        assert_eq!(reply.status, 200, "{id}: {}", reply.body);
    }

    // A request rule, and the loopback toggle on for acme alone.
    created(
        &api1,
        "/api/request-rules",
        json!({"workspace": "acme", "host": "api.example.test", "methods": ["DELETE"], "path": "/v1/**"}),
    )
    .await;
    let toggles = json!({"overrides": {"local_toggles": {
        "loopback": true, "private": null, "link_local": null,
        "metadata": null, "special": null, "company_network": null,
    }}});
    let reply = api1
        .put("/api/settings/workspaces/acme", &toggles.to_string())
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);

    // Let the expiring rule run out, then take every decision once before the restart.
    tokio::time::sleep(Duration::from_millis(
        soon.saturating_sub(now_ms()).saturating_add(700),
    ))
    .await;
    start_and_wait(&api1, &mut events, "acme").await;
    assert_decisions(&rig, "before the restart").await;
    let rules_before = without_expired(api1.get("/api/rules").await.json());
    let sets_before = api1.get("/api/rule-sets").await.json()["sets"].clone();
    let request_rules_before = api1.get("/api/request-rules").await.json();
    let settings_before = api1.get("/api/settings/workspaces/acme").await.json();

    host.shutdown().await;
    drop(host);
    drop(events);

    // A new process on the same data folder.
    let again = rig.start().await;
    let api2 = api(&again);
    let mut events = api2.events().await;
    start_and_wait(&api2, &mut events, "acme").await;
    start_and_wait(&api2, &mut events, "beta").await;
    assert_decisions(&rig, "after the restart").await;

    // The lists the screens read are the ones from before.
    let listed_after = api2.get("/api/rules").await.json();
    assert!(
        !listed_after.to_string().contains("soon.test"),
        "the expired rule must stay gone: {listed_after}"
    );
    assert_eq!(without_expired(listed_after), rules_before);
    assert_eq!(api2.get("/api/rule-sets").await.json()["sets"], sets_before);
    assert_eq!(
        api2.get("/api/request-rules").await.json(),
        request_rules_before
    );
    assert_eq!(
        api2.get("/api/settings/workspaces/acme").await.json(),
        settings_before
    );
    again.shutdown().await;
}

async fn start_and_wait(api: &Api, events: &mut support::Events, workspace: &'static str) {
    if status_of(&api.get(&format!("/api/workspaces/{workspace}")).await) == "running" {
        return;
    }
    api.post(&format!("/api/workspaces/{workspace}/start"), "")
        .await;
    let end = events
        .until(ended(workspace), Duration::from_secs(20))
        .await;
    assert_eq!(end["step"], "done", "{end}");
}

/// The rule list without the rule that ran out: the sweeper may or may not have removed it from
/// the first process's list yet, and that timing is not what this test checks.
fn without_expired(mut list: serde_json::Value) -> serde_json::Value {
    list["rules"]
        .as_array_mut()
        .unwrap()
        .retain(|rule| rule["pattern"] != "soon.test");
    list
}
