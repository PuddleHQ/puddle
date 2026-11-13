// SPDX-License-Identifier: GPL-3.0-or-later
//! The Copilot session token through the real proxy: the extension trades the stand-in GitHub
//! token for a session token with a `GET`, the workspace gets a stand-in with the real token's
//! fields and a random MAC, and the real token goes out only toward the Copilot hosts. A hostile
//! guest finds the real token in none of the answers, the audit rows or the logs.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use http_body_util::BodyExt as _;
use puddle_logins::{LoginNotices, Logins, Problem, Profile, builtin};
use puddle_proxy::{InjectDecision, SecretValue, StandIn, StandInOrigin, StandIns};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use terminate_support::h2_rig::{H2Server, Reply as H2Reply, Script, full};
use terminate_support::{
    Client, FakeServer, Flaw, Handler, Pki, Recorded, Reply, Rig, RigBuilder, TestInjector,
    captured_logs,
};

const API: &str = "api.github.com";
const COPILOT: &str = "api.individual.githubcopilot.com";
const GH_TOKEN: &str = "gho_CANARYgithub0123456789abcdefghijkl";
const FIELDS: &str = "tid=7a1b;exp=1790000000;sku=free_limited_copilot;proxy-ep=proxy.individual.githubcopilot.com;chat=1";

/// The real session token of the `n`th exchange: its own id field and its own MAC.
fn real_session(n: usize) -> String {
    format!("{FIELDS};n={n}:{n:x}badc0ffee0badc0ffee0badc0ffee0badc0ffee0badc0ffee0badc0ffee0ba")
}

fn mac_of(token: &str) -> &str {
    token.split_once(':').unwrap().1
}

fn assert_no_real_session(text: &str) {
    for n in 1..=9 {
        let real = real_session(n);
        assert!(
            !text.contains(mac_of(&real)),
            "the MAC of {n} leaked: {text}"
        );
    }
    assert!(!text.contains("badc0ffee"), "a canary leaked: {text}");
    assert!(!text.contains(GH_TOKEN), "the GitHub token leaked: {text}");
}

#[derive(Default)]
struct Notices(Mutex<Vec<Problem>>);

impl LoginNotices for Notices {
    fn problem(&self, _: &WorkspaceId, _: &Profile, problem: Problem) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(problem);
    }
}

fn session_json(n: usize) -> Value {
    json!({
        "token": real_session(n),
        "expires_at": 1_790_000_000,
        "refresh_in": 1500,
        "endpoints": {"api": format!("https://{COPILOT}")},
    })
}

/// What the stand-in `gho_` token and the session logins registry hold when the extension asks.
fn registry_with_login() -> (Arc<StandIns>, Logins, Arc<Notices>) {
    let notices = Arc::new(Notices::default());
    let registry = Arc::new(StandIns::new());
    let logins = Logins::new(
        WorkspaceId::new("box").unwrap(),
        true,
        builtin(),
        Arc::clone(&registry),
        Arc::new(MemoryStore::new()) as Arc<dyn SecretStore>,
        Arc::clone(&notices) as Arc<dyn LoginNotices>,
        puddle_logins::Marker::none(),
    );
    (registry, logins, notices)
}

fn gho_stand_in(registry: &StandIns) -> String {
    let stand_in = "gho_STANDINstandin0123456789abcdefghij".to_owned();
    registry
        .insert(
            StandIn::new(
                StandInOrigin::CapturedLogin,
                "github-access",
                &stand_in,
                SecretValue::new(GH_TOKEN),
                puddle_logins::GITHUB.hosts(),
            )
            .unwrap(),
        )
        .unwrap();
    stand_in
}

fn api_handler() -> Handler {
    let exchanges = Arc::new(AtomicUsize::new(0));
    Arc::new(move |request: &Recorded| {
        let asked = request.method == "GET"
            && request.target == "/copilot_internal/v2/token"
            && request.header("authorization") == Some(&format!("token {GH_TOKEN}"));
        if !asked {
            return Reply::status(401, "content-type: application/json\r\n", "{}");
        }
        let n = exchanges.fetch_add(1, Ordering::SeqCst) + 1;
        let body = session_json(n).to_string();
        Reply::raw(format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        ))
    })
}

fn request(method: &str, host: &str, path: &str, extra: &str) -> Vec<u8> {
    format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: 0\r\n\r\n")
        .into_bytes()
}

fn fields_of(token: &str) -> &str {
    token.split_once(':').unwrap().0
}

/// A proxy in front of the three fake hosts, with the stand-in `gho_` token registered.
struct Session {
    api: FakeServer,
    copilot: FakeServer,
    elsewhere: FakeServer,
    rig: Rig,
    gho: String,
    notices: Arc<Notices>,
    _registry: Arc<StandIns>,
}

async fn session() -> Session {
    let pki = Pki::new();
    let api = FakeServer::tls(pki.server_config(API, Flaw::None), api_handler()).await;
    let copilot = FakeServer::tls(
        pki.server_config(COPILOT, Flaw::None),
        Arc::new(|_: &Recorded| Reply::ok("hello")),
    )
    .await;
    let elsewhere = FakeServer::tls(
        pki.server_config("elsewhere.test", Flaw::None),
        Arc::new(|_: &Recorded| Reply::ok("x")),
    )
    .await;
    let (registry, logins, notices) = registry_with_login();
    let gho = gho_stand_in(&registry);
    let hosts = vec![API, COPILOT, "elsewhere.test"];
    let rig = RigBuilder::new(&pki)
        .bound(hosts.clone())
        .allow(hosts)
        .name(API, api.addr)
        .name(COPILOT, copilot.addr)
        .name("elsewhere.test", elsewhere.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins.clone()))
        .build();
    Session {
        api,
        copilot,
        elsewhere,
        rig,
        gho,
        notices,
        _registry: registry,
    }
}

impl Session {
    /// Two exchanges, as the extension makes them: the extension's `GET` with the stand-in
    /// `gho_` token in `Authorization: token ...`. Returns the two session stand-ins.
    async fn two_exchanges(&self, token_client: &mut Client) -> Vec<String> {
        let mut given = Vec::new();
        for n in 1..=2 {
            token_client
                .send(&request(
                    "GET",
                    API,
                    "/copilot_internal/v2/token",
                    &format!(
                        "Authorization: token {}\r\nX-GitHub-Api-Version: 2025-04-01\r\n",
                        self.gho
                    ),
                ))
                .await;
            let response = token_client.response("GET").await;
            assert_eq!(response.status, 200, "{}", response.text());
            assert_no_real_session(&response.text());
            let answer: Value = serde_json::from_slice(&response.body).unwrap();
            let token = answer.get("token").unwrap().as_str().unwrap().to_owned();
            // What the extension reads: the same fields, the same envelope, a MAC of the same length.
            assert_eq!(fields_of(&token), fields_of(&real_session(n)));
            assert_eq!(mac_of(&token).len(), mac_of(&real_session(n)).len());
            assert_eq!(answer.get("refresh_in"), Some(&json!(1500)));
            assert_eq!(
                answer.pointer("/endpoints/api"),
                Some(&json!(format!("https://{COPILOT}")))
            );
            assert_eq!(
                response.header("content-length"),
                Some(response.body.len().to_string().as_str())
            );
            given.push(token);
        }
        given
    }

    /// What no test may leave behind: the real token in the audit rows, the logs or the notices.
    async fn finish(self, clients: Vec<Client>) {
        for client in clients {
            client.close().await;
        }
        let events = self.rig.events(1).await;
        assert_no_real_session(&format!("{events:?}"));
        assert_no_real_session(&captured_logs());
        assert!(self.notices.0.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn hostile_the_real_copilot_session_token_never_reaches_the_guest_in_two_exchanges() {
    let s = session().await;
    let mut guest = s.rig.guest();
    let mut token_client = guest.tls(&format!("{API}:443"), None).await.unwrap();

    let given = s.two_exchanges(&mut token_client).await;
    assert_ne!(given[0], given[1], "a new stand-in at every exchange");
    assert_eq!(
        s.api.recorded()[0].header("authorization"),
        Some(format!("token {GH_TOKEN}").as_str()),
        "the real GitHub token went out in the request"
    );

    s.finish(vec![token_client]).await;
}

#[tokio::test]
async fn hostile_both_session_stand_ins_work_toward_the_copilot_host_each_with_its_real_token() {
    let s = session().await;
    let mut guest = s.rig.guest();
    let mut token_client = guest.tls(&format!("{API}:443"), None).await.unwrap();
    let given = s.two_exchanges(&mut token_client).await;

    // Both stand-ins work toward the Copilot host, each with the real token of its exchange.
    let mut chat = guest.tls(&format!("{COPILOT}:443"), None).await.unwrap();
    for (n, stand_in) in given.iter().enumerate() {
        chat.send(&request(
            "GET",
            COPILOT,
            "/models",
            &format!("Authorization: Bearer {stand_in}\r\n"),
        ))
        .await;
        assert_eq!(chat.response("GET").await.status, 200);
        assert_eq!(
            s.copilot.recorded()[n].header("authorization"),
            Some(format!("Bearer {}", real_session(n + 1)).as_str())
        );
    }

    s.finish(vec![token_client, chat]).await;
}

#[tokio::test]
async fn hostile_a_session_stand_in_toward_a_host_that_is_not_the_service_goes_out_as_text() {
    let s = session().await;
    let mut guest = s.rig.guest();
    let mut token_client = guest.tls(&format!("{API}:443"), None).await.unwrap();
    let given = s.two_exchanges(&mut token_client).await;

    // A stand-in toward a decrypted host that is not the service's goes out as the text it is.
    let mut away = guest.tls("elsewhere.test:443", None).await.unwrap();
    away.send(&request(
        "GET",
        "elsewhere.test",
        "/",
        &format!("Authorization: Bearer {}\r\n", given[1]),
    ))
    .await;
    assert_eq!(away.response("GET").await.status, 200);
    away.close().await;
    assert_eq!(
        s.elsewhere.recorded()[0].header("authorization"),
        Some(format!("Bearer {}", given[1]).as_str())
    );

    s.finish(vec![token_client]).await;
}

#[tokio::test]
async fn hostile_a_made_up_session_token_with_a_real_ones_fields_gets_nothing_swapped() {
    let s = session().await;
    let mut guest = s.rig.guest();
    let mut token_client = guest.tls(&format!("{API}:443"), None).await.unwrap();
    let given = s.two_exchanges(&mut token_client).await;
    let mut chat = guest.tls(&format!("{COPILOT}:443"), None).await.unwrap();

    // A guest that makes up a session token with a real one's fields gets nothing swapped.
    let forged = format!("{}:{}", fields_of(&given[1]), "0".repeat(64));
    chat.send(&request(
        "GET",
        COPILOT,
        "/models",
        &format!("Authorization: Bearer {forged}\r\n"),
    ))
    .await;
    assert_eq!(chat.response("GET").await.status, 200);
    assert_eq!(
        s.copilot.recorded().last().unwrap().header("authorization"),
        Some(format!("Bearer {forged}").as_str())
    );

    s.finish(vec![token_client, chat]).await;
}

#[tokio::test]
async fn over_http2_the_copilot_session_token_is_captured_the_same_way() {
    let pki = Pki::new();
    let exchanges = Arc::new(AtomicUsize::new(0));
    let api_script: Script = Arc::new(move |_| {
        let n = exchanges.fetch_add(1, Ordering::SeqCst) + 1;
        let reply: H2Reply = http::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(
                http_body_util::Full::new(Bytes::from(session_json(n).to_string()))
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        reply
    });
    let api = H2Server::recording(&pki, API, api_script).await;
    let copilot_script: Script = Arc::new(|_| terminate_support::h2_rig::reply(200, "ok"));
    let copilot = H2Server::recording(&pki, COPILOT, copilot_script).await;
    let (registry, logins, notices) = registry_with_login();
    let gho = gho_stand_in(&registry);
    let rig = RigBuilder::new(&pki)
        .bound(vec![API, COPILOT])
        .allow(vec![API, COPILOT])
        .name(API, api.addr)
        .name(COPILOT, copilot.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins))
        .build();
    let mut guest = rig.guest();
    let mut token_client = guest.h2(&format!("{API}:443"), &[b"h2"]).await;
    let got = token_client
        .request(
            "GET",
            API,
            "/copilot_internal/v2/token",
            &[("authorization", &format!("token {gho}"))],
            full(Bytes::new()),
        )
        .await;
    assert_eq!(got.status, 200);
    assert_no_real_session(&got.text());
    let answer: Value = serde_json::from_slice(&got.body).unwrap();
    let stand_in = answer["token"].as_str().unwrap().to_owned();
    assert_eq!(fields_of(&stand_in), fields_of(&real_session(1)));
    assert_eq!(
        got.header("content-length"),
        Some(got.body.len().to_string().as_str())
    );
    assert_eq!(
        api.recorded()[0].header("authorization"),
        Some(format!("token {GH_TOKEN}").as_str())
    );

    let mut chat = guest.h2(&format!("{COPILOT}:443"), &[b"h2"]).await;
    let call = chat
        .request(
            "GET",
            COPILOT,
            "/models",
            &[("authorization", &format!("Bearer {stand_in}"))],
            full(Bytes::new()),
        )
        .await;
    assert_eq!(call.status, 200);
    assert_eq!(
        copilot.recorded()[0].header("authorization"),
        Some(format!("Bearer {}", real_session(1)).as_str())
    );
    assert_no_real_session(&captured_logs());
    assert!(notices.0.lock().unwrap().is_empty());
}
