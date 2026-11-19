// SPDX-License-Identifier: AGPL-3.0-or-later
//! A guest behind a company proxy that Puddle found from the environment, against a real Squid
//! with Basic proxy authentication: what the network-health report says about it, what the guest
//! meets when the proxy wants a sign-in or is down, and that a sign-in entered through the API
//! works for the next request with no restart. Everything between the guest's `CONNECT` and Squid
//! is the real code (route, proxy, upstream chain, sign-in store, API).
//!
//! Squid comes from `PUDDLE_SQUID_BIN` or `PUDDLE_SQUID_DOCKER`; with neither the tests skip,
//! unless `PUDDLE_REQUIRE_SQUID=1` (CI sets it).

use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr};

use futures_util::StreamExt as _;
use puddle_agent_proto::tokio_yamux::Session;
use puddle_e2e::lab::{BASIC_USER, Squid, SquidOptions, basic_password};
use puddle_proxy::testing::StaticResolver;
use puddle_upstream::{Config as DiscoveryConfig, EnvOs};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

/// The name Squid resolves to the echo server (`SquidOptions::basic`). The host's own resolver
/// maps it to loopback too, and the workspace may reach loopback, so a connection that went around
/// the proxy would reach the echo server directly and be counted there.
const TARGET: &str = "echo.corp.test";

/// A TCP echo server on loopback and the number of connections it accepted.
struct Echo {
    port: u16,
    accepted: Arc<AtomicUsize>,
}

async fn echo_server() -> Echo {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (mut read, mut write) = conn.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    Echo { port, accepted }
}

/// Points the rig's host at `squid` the way a company machine does: `HTTP_PROXY` and
/// `HTTPS_PROXY` in the environment (the operating system layer is Unix's own), with `TARGET`
/// resolvable by the host itself.
fn behind_env_squid(rig: &Rig, squid: &Squid) {
    let proxy = format!("http://127.0.0.1:{}", squid.port());
    let env = EnvOs::from_vars([
        (OsString::from("HTTP_PROXY"), OsString::from(&proxy)),
        (OsString::from("HTTPS_PROXY"), OsString::from(&proxy)),
    ]);
    let discovery = Discovery::new(Arc::new(env), DiscoveryConfig::default());
    let resolver = Arc::new(StaticResolver::new().with(TARGET, &[IpAddr::V4(Ipv4Addr::LOCALHOST)]));
    *rig.network.lock().unwrap() = Some((discovery, resolver));
}

/// What a guest's `CONNECT` met: the head of the answer (status line and headers), the body of a
/// refusal, and, when the tunnel opened, whether a round trip through it came back byte for byte.
struct Met {
    head: String,
    body: String,
    echoed: bool,
}

impl Met {
    fn status(&self) -> &str {
        self.head.lines().next().unwrap_or_default()
    }
}

/// One `CONNECT TARGET:port` from `workspace`'s guest over the workspace's proxy route, then
/// `ping` through the tunnel when it opens.
async fn guest_connect(rig: &Rig, workspace: &str, port: u16) -> Met {
    let sandbox = WorkspaceId::new(workspace).unwrap().sandbox_name();
    let routes = rig.runtime.routes(&sandbox);
    let conn = puddle_ipc::connect(&routes.first().unwrap().host)
        .await
        .unwrap();
    let mut session = Session::new_client(conn, puddle_agent_proto::yamux::client_config());
    let mut control = session.control();
    let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
    let mut stream = control.open_stream().await.unwrap();
    let authority = format!("{TARGET}:{port}");
    let request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let met = tokio::time::timeout(Duration::from_secs(20), async {
        let mut reply = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !reply.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => reply.extend_from_slice(&chunk[..n]),
            }
        }
        let text = String::from_utf8_lossy(&reply).into_owned();
        let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let mut met = Met {
            head: head.to_owned(),
            body: rest.to_owned(),
            echoed: false,
        };
        if met.status().starts_with("HTTP/1.1 200") {
            stream.write_all(b"ping").await.unwrap();
            let mut back = [0_u8; 4];
            met.echoed = stream.read_exact(&mut back).await.is_ok() && &back == b"ping";
        } else {
            // The refusal's body follows its head; the stream ends after it.
            let mut more = Vec::new();
            let _ = stream.read_to_end(&mut more).await;
            met.body.push_str(&String::from_utf8_lossy(&more));
        }
        met
    })
    .await
    .expect("no answer to the guest's CONNECT");
    driver.abort();
    met
}

fn blocked_by(met: &Met) -> Option<String> {
    met.head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("x-puddle-blocked-by")
            .then(|| value.trim().to_owned())
    })
}

/// The audit's connection records for `host`, as the store wrote them.
fn connection_records(host: &Host<FakeRuntime>, name: &str) -> Vec<serde_json::Value> {
    host.store()
        .audit_lines(0, 100_000)
        .unwrap()
        .into_iter()
        .filter_map(|(_, line)| serde_json::from_str::<serde_json::Value>(&line).ok())
        .filter(|v| v["type"] == "connection" && v["host"] == name)
        .collect()
}

/// A started rig host with workspace `acme` and a rule that lets it reach `TARGET`.
async fn host_with_acme(rig: &Rig) -> Host<FakeRuntime> {
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let rule = api
        .post(
            "/api/rules",
            &json!({"scope": {"type": "global"}, "pattern": TARGET, "effect": "allow"}).to_string(),
        )
        .await;
    assert_eq!(rule.status, 201, "{}", rule.body);
    // The workspace may reach loopback, so the guard that stops a connection to the echo server
    // would not be what stops a direct one: only the route does.
    let toggles = json!({"overrides": {"local_toggles": {
        "loopback": true, "private": null, "link_local": null,
        "metadata": null, "special": null, "company_network": null,
    }}});
    let saved = api
        .put("/api/settings/workspaces/acme", &toggles.to_string())
        .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    host
}

#[tokio::test(flavor = "multi_thread")]
async fn the_network_health_report_names_the_proxy_found_in_the_environment() {
    let echo = echo_server().await;
    let Some(squid) = Squid::start(&SquidOptions::basic(&[echo.port])) else {
        return;
    };
    let rig = Rig::named_ids();
    behind_env_squid(&rig, &squid);
    let host = rig.start().await;

    let report = api(&host).get("/api/network-health").await.json();
    let proxy = &report["proxy"];
    assert_eq!(proxy["detected"], "env", "{report}");
    assert_eq!(proxy["mode"], "system", "{report}");
    let address = format!("127.0.0.1:{}", squid.port());
    assert_eq!(proxy["https_proxy"], address, "{report}");
    assert_eq!(proxy["http_proxy"], address, "{report}");
    assert_eq!(proxy["pac_url"], json!(null), "{report}");

    // The screen's scenario for this setup (`ui/e2e/fixtures/company-proxy-env.json`) is the same
    // report with the lab's address: whatever the host adds or renames fails here first.
    let scenario: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../ui/e2e/fixtures/company-proxy-env.json"
    ))
    .unwrap();
    let shown = &scenario["network_health"]["proxy"];
    let keys = |value: &serde_json::Value| -> Vec<String> {
        value.as_object().unwrap().keys().cloned().collect()
    };
    assert_eq!(keys(shown), keys(proxy), "the scenario's proxy section");
    for key in ["detected", "mode", "pac_state", "auto_detect"] {
        assert_eq!(shown[key], proxy[key], "{key} in the scenario");
    }
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_is_refused_for_want_of_a_sign_in_and_tunnels_after_one_is_entered_with_no_restart()
{
    let echo = echo_server().await;
    let Some(squid) = Squid::start(&SquidOptions::basic(&[echo.port])) else {
        return;
    };
    let rig = Rig::named_ids();
    behind_env_squid(&rig, &squid);
    let host = host_with_acme(&rig).await;
    let api = api(&host);

    // Squid asks for a sign-in and Puddle has none.
    let before = guest_connect(&rig, "acme", echo.port).await;
    assert_eq!(
        before.status(),
        "HTTP/1.1 502 Bad Gateway",
        "{}",
        before.head
    );
    assert_eq!(blocked_by(&before).as_deref(), Some("upstream-auth"));
    assert!(
        before
            .body
            .contains("requires sign-in that Puddle cannot provide"),
        "{}",
        before.body
    );
    assert!(!squid.access_log().contains("TCP_TUNNEL"));

    // The user keeps the sign-in in Settings; the same host, the next request.
    let address = format!("127.0.0.1:{}", squid.port());
    let password = basic_password();
    let put = api
        .put(
            "/api/proxy-sign-ins",
            &json!({"proxy": address, "user": BASIC_USER, "password": password}).to_string(),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    let after = guest_connect(&rig, "acme", echo.port).await;
    assert!(after.status().starts_with("HTTP/1.1 200"), "{}", after.head);
    assert!(after.echoed, "a round trip through the tunnel");

    let log = squid.access_log_with("TCP_TUNNEL/200");
    assert!(
        log.contains("TCP_TUNNEL/200") && log.contains(&format!("{BASIC_USER} ")),
        "Squid's log shows the tunnel for {BASIC_USER}:\n{log}"
    );
    fixture::assert_absent("the guest's answers", &after.head, &password);
    fixture::assert_absent("the guest's answers", &before.body, &password);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_is_told_the_company_proxy_is_down_never_goes_around_it_and_works_when_it_is_back()
{
    let echo = echo_server().await;
    let Some(mut squid) = Squid::start(&SquidOptions::basic(&[echo.port])) else {
        return;
    };
    let rig = Rig::named_ids();
    behind_env_squid(&rig, &squid);
    let host = host_with_acme(&rig).await;
    host.proxy_passwords()
        .set(
            &puddle_upstream::ProxyAddr::new("127.0.0.1", squid.port()),
            BASIC_USER,
            &basic_password(),
        )
        .unwrap();

    let up = guest_connect(&rig, "acme", echo.port).await;
    assert!(up.echoed, "{}", up.head);
    let reached = echo.accepted.load(Ordering::SeqCst);

    squid.pause();
    let down = guest_connect(&rig, "acme", echo.port).await;
    assert_eq!(down.status(), "HTTP/1.1 502 Bad Gateway", "{}", down.head);
    assert!(
        down.body.contains("company proxy did not answer"),
        "the guest's text names the company proxy: {}",
        down.body
    );
    assert!(!down.body.contains("no route to it works"), "{}", down.body);
    assert!(
        !down.body.contains(&format!("127.0.0.1:{}", squid.port())),
        "the proxy's address stays in the log: {}",
        down.body
    );
    assert_eq!(
        echo.accepted.load(Ordering::SeqCst),
        reached,
        "nothing went around the company proxy to the target the host can resolve itself"
    );
    let records = connection_records(&host, TARGET);
    assert_eq!(
        records.last().map(|r| r["outcome"].clone()),
        Some(json!("connect_failed")),
        "{records:?}"
    );

    squid.resume();
    let back = guest_connect(&rig, "acme", echo.port).await;
    assert!(back.status().starts_with("HTTP/1.1 200"), "{}", back.head);
    assert!(back.echoed, "a round trip through the tunnel");
    host.shutdown().await;
}
