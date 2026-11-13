// SPDX-License-Identifier: GPL-3.0-or-later
//! The Docker Hub login through the real proxy and the shipped profile, against fake servers that
//! speak the CLI's wire format: the device flow's poll (a form in, JSON out), the personal access token
//! `desktop-generate` mints from the access token (a nested `data.token`), the `Basic` credential the
//! token is sent in toward `auth.docker.io`, the registry's bearer token that server answers with
//! (sent on to the registry), and the revoke that carries the refresh token back (an empty answer).
//! The real tokens reach only the hosts they belong to.
use crate::terminate_support;

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use puddle_logins::{LoginNotices, Logins, Problem, Profile, builtin};
use puddle_proxy::{InjectDecision, StandIns};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use terminate_support::{
    FakeServer, Flaw, Handler, Pki, Recorded, Reply, RigBuilder, TestInjector,
};

const LOGIN: &str = "login.docker.com";
const HUB: &str = "hub.docker.com";
const REGISTRY_AUTH: &str = "auth.docker.io";
const REGISTRY: &str = "registry-1.docker.io";
const REFRESH: &str = "v1.CANARY0docker0refresh0AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PAT: &str = "dckr_pat_CANARY0docker0pat0AAAAAAA";

fn jwt(claims: &Value, fill: char) -> String {
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
        fill.to_string().repeat(342),
    )
}

fn real_access() -> String {
    jwt(
        &json!({"exp": 1_893_456_000, "https://hub.docker.com": {"username": "someone"}}),
        'A',
    )
}

fn real_id() -> String {
    jwt(&json!({"sub": "auth0|1"}), 'I')
}

/// The registry's bearer token, with the claims the live token server puts in one.
fn real_bearer() -> String {
    jwt(
        &json!({"iss": "auth.docker.io", "aud": "registry.docker.io", "sub": "someone", "exp": 1_893_456_300,
            "access": [{"type": "repository", "name": "someone/app", "actions": ["pull"]}]}),
        'B',
    )
}

struct Quiet(Mutex<Vec<Problem>>);

impl LoginNotices for Quiet {
    fn problem(&self, _: &WorkspaceId, _: &Profile, problem: Problem) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(problem);
    }
}

fn reply(status: &str, body: &str) -> Reply {
    Reply::raw(format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    ))
}

/// The login host: the device flow's poll, and a revoke that wants the real refresh token and
/// answers `200` with nothing at all (no content type), as Auth0 does.
fn login_handler() -> Handler {
    Arc::new(|request: &Recorded| {
        let text = String::from_utf8_lossy(&request.body).into_owned();
        match request.target.as_str() {
            "/oauth/token" if text.contains("device_code") => reply(
                "200 OK",
                &json!({"access_token": real_access(), "id_token": real_id(), "refresh_token": REFRESH, "expires_in": 86400, "token_type": "Bearer"}).to_string(),
            ),
            "/oauth/revoke" if text.contains(&format!("token={REFRESH}")) => {
                Reply::raw("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_owned())
            }
            _ => reply("403 Forbidden", r#"{"error":"access_denied"}"#),
        }
    })
}

/// Hub: `desktop-generate` wants the real access token as a bearer and answers `201`.
fn hub_handler() -> Handler {
    Arc::new(|request: &Recorded| {
        if request.method == "POST"
            && request.target == "/v2/access-tokens/desktop-generate"
            && request.header("authorization") == Some(format!("Bearer {}", real_access()).as_str())
        {
            return reply(
                "201 Created",
                &json!({"data": {"uuid": "u-1", "token": PAT, "token_label": "desktop"}})
                    .to_string(),
            );
        }
        reply("401 Unauthorized", r#"{"message":"no"}"#)
    })
}

fn post(host: &str, path: &str, content_type: &str, extra: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn docker_logs_in_makes_a_pat_logs_into_the_registry_and_revokes_with_stand_ins_only() {
    let pki = Pki::new();
    let login = FakeServer::tls(pki.server_config(LOGIN, Flaw::None), login_handler()).await;
    let hub = FakeServer::tls(pki.server_config(HUB, Flaw::None), hub_handler()).await;
    let registry_auth = FakeServer::tls(
        pki.server_config(REGISTRY_AUTH, Flaw::None),
        Arc::new(|_| {
            let bearer = real_bearer();
            reply(
                "200 OK",
                &json!({"token": bearer, "access_token": bearer, "expires_in": 300}).to_string(),
            )
        }),
    )
    .await;
    let registry_server = FakeServer::tls(
        pki.server_config(REGISTRY, Flaw::None),
        Arc::new(|_| reply("200 OK", "{}")),
    )
    .await;
    let registry = Arc::new(StandIns::new());
    let notices = Arc::new(Quiet(Mutex::new(Vec::new())));
    let logins = Logins::new(
        WorkspaceId::new("box").unwrap(),
        true,
        builtin(),
        Arc::clone(&registry),
        Arc::new(MemoryStore::new()) as Arc<dyn SecretStore>,
        Arc::clone(&notices) as Arc<dyn LoginNotices>,
        puddle_logins::Marker::none(),
    );
    let rig = RigBuilder::new(&pki)
        .bound(vec![LOGIN, HUB, REGISTRY_AUTH, REGISTRY])
        .allow(vec![LOGIN, HUB, REGISTRY_AUTH, REGISTRY])
        .name(LOGIN, login.addr)
        .name(HUB, hub.addr)
        .name(REGISTRY_AUTH, registry_auth.addr)
        .name(REGISTRY, registry_server.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins))
        .build();
    let mut guest = rig.guest();

    // The device flow's poll.
    let mut client = guest.tls(&format!("{LOGIN}:443"), None).await.unwrap();
    client
        .send(&post(
            LOGIN,
            "/oauth/token",
            "application/x-www-form-urlencoded",
            "",
            "client_id=app&grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code=d",
        ))
        .await;
    let response = client.response("POST").await;
    assert_eq!(response.status, 200);
    let text = String::from_utf8(response.body.clone()).unwrap();
    for real in [real_access(), real_id(), REFRESH.to_owned()] {
        assert!(!text.contains(&real), "a real token reached the guest");
    }
    let given: Value = serde_json::from_str(&text).unwrap();
    let access = given["access_token"].as_str().unwrap().to_owned();
    let refresh = given["refresh_token"].as_str().unwrap().to_owned();
    assert_eq!(access.split('.').nth(1), real_access().split('.').nth(1));

    // The PAT: the access token's stand-in is the real bearer upstream; the answer's `data.token`
    // is a stand-in, `201` and the rest of the answer as it was.
    let bearer = format!("Authorization: Bearer {access}\r\n");
    let mut hub_client = guest.tls(&format!("{HUB}:443"), None).await.unwrap();
    hub_client
        .send(&post(
            HUB,
            "/v2/access-tokens/desktop-generate",
            "application/json",
            &bearer,
            "",
        ))
        .await;
    let response = hub_client.response("POST").await;
    assert_eq!(
        response.status,
        201,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    assert!(!String::from_utf8_lossy(&response.body).contains(PAT));
    let made: Value = serde_json::from_slice(&response.body).unwrap();
    let pat = made["data"]["token"].as_str().unwrap().to_owned();
    assert!(pat.starts_with("dckr_pat_"));
    assert_eq!(made["data"]["uuid"], "u-1");
    hub_client.close().await;

    // The registry login: `Basic user:pat` reaches the auth server with the real PAT.
    let basic = STANDARD.encode(format!("someone:{pat}"));
    let mut auth_client = guest
        .tls(&format!("{REGISTRY_AUTH}:443"), None)
        .await
        .unwrap();
    auth_client
        .send(
            format!(
                "GET /token?service=registry.docker.io HTTP/1.1\r\nHost: {REGISTRY_AUTH}\r\nAuthorization: Basic {basic}\r\nContent-Length: 0\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
    let response = auth_client.response("GET").await;
    assert_eq!(response.status, 200);
    // The registry's bearer token the server answered with reaches the guest as a stand-in only.
    let text = String::from_utf8(response.body.clone()).unwrap();
    assert!(
        !text.contains(&"B".repeat(342)),
        "a real bearer reached the guest"
    );
    let answered: Value = serde_json::from_str(&text).unwrap();
    let bearer_stand_in = answered["token"].as_str().unwrap().to_owned();
    assert_eq!(answered["access_token"], answered["token"]);
    let seen = registry_auth.recorded();
    let sent = seen.last().unwrap().header("authorization").unwrap();
    let decoded = STANDARD
        .decode(sent.strip_prefix("Basic ").unwrap())
        .unwrap();
    assert_eq!(
        String::from_utf8(decoded).unwrap(),
        format!("someone:{PAT}")
    );
    auth_client.close().await;

    // The pull: the stand-in is the real bearer at the registry.
    let mut pull = guest.tls(&format!("{REGISTRY}:443"), None).await.unwrap();
    pull.send(
        format!(
            "GET /v2/someone/app/manifests/latest HTTP/1.1\r\nHost: {REGISTRY}\r\nAuthorization: Bearer {bearer_stand_in}\r\nContent-Length: 0\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(pull.response("GET").await.status, 200);
    assert_eq!(
        registry_server
            .recorded()
            .last()
            .unwrap()
            .header("authorization"),
        Some(format!("Bearer {}", real_bearer()).as_str())
    );
    pull.close().await;

    // The revoke (`docker logout`): the refresh token's stand-in is the real one upstream, and the
    // empty answer is not an unexpected answer.
    client
        .send(&post(
            LOGIN,
            "/oauth/revoke",
            "application/x-www-form-urlencoded",
            "",
            &format!("client_id=app&token={refresh}"),
        ))
        .await;
    let response = client.response("POST").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"");
    client.close().await;
    assert!(notices.0.lock().unwrap().is_empty());
    // The refresh token's stand-in never reached the login host.
    assert!(
        !login
            .recorded()
            .iter()
            .any(|r| { String::from_utf8_lossy(&r.body).contains(&refresh) })
    );
}
