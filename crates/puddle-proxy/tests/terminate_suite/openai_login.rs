// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `OpenAI` Codex login through the real proxy and the shipped profile, against fake servers that
//! speak the login's wire format: the code exchange (a form), the token exchange that trades the
//! identity token for an API key (a form again, same path and same answer field), the refresh
//! (JSON), and the API calls. The real tokens reach only the hosts they belong to.
use crate::terminate_support;

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use puddle_logins::{LoginNotices, Logins, Problem, Profile, builtin};
use puddle_proxy::{InjectDecision, StandIns};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use terminate_support::{
    FakeServer, Flaw, Handler, Pki, Recorded, Reply, RigBuilder, TestInjector,
};

const AUTH: &str = "auth.openai.com";
const API: &str = "api.openai.com";
const CHATGPT: &str = "chatgpt.com";
const REFRESH: &str = "rt_CANARY0openai0refresh.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const API_KEY: &str =
    "sk-proj-CANARY0openai0api0key0AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn jwt(claims: &Value, fill: char) -> String {
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
        fill.to_string().repeat(342),
    )
}

fn real_id() -> String {
    jwt(
        &json!({"email": "me@example.test", "exp": 1_893_456_000}),
        'I',
    )
}

fn real_access() -> String {
    jwt(&json!({"exp": 1_893_456_000, "pad": "p".repeat(1500)}), 'A')
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

fn json_reply(body: &Value) -> Reply {
    let body = body.to_string();
    Reply::raw(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    ))
}

/// The auth server: a code exchange, a token exchange that wants the real identity token, and a
/// refresh that wants the real refresh token. Anything else is refused.
fn auth_handler() -> Handler {
    Arc::new(|request: &Recorded| {
        let text = String::from_utf8_lossy(&request.body).into_owned();
        if request.method != "POST" || request.target != "/oauth/token" {
            return Reply::status(404, "", "");
        }
        if text.contains("grant_type=authorization_code") {
            return json_reply(
                &json!({"id_token": real_id(), "access_token": real_access(), "refresh_token": REFRESH}),
            );
        }
        if text.contains("token-exchange") && text.contains(&format!("subject_token={}", real_id()))
        {
            return json_reply(
                &json!({"access_token": API_KEY, "token_type": "Bearer", "issued_token_type": "urn:ietf:params:oauth:token-type:access_token"}),
            );
        }
        if let Ok(body) = serde_json::from_str::<Value>(&text)
            && body.get("grant_type").and_then(Value::as_str) == Some("refresh_token")
            && body.get("refresh_token").and_then(Value::as_str) == Some(REFRESH)
        {
            return json_reply(
                &json!({"id_token": real_id(), "access_token": real_access(), "refresh_token": REFRESH}),
            );
        }
        Reply::status(
            400,
            "content-type: application/json\r\n",
            r#"{"error":"invalid_grant"}"#,
        )
    })
}

fn post(host: &str, content_type: &str, body: &str) -> Vec<u8> {
    format!(
        "POST /oauth/token HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn get(host: &str, token: &str) -> Vec<u8> {
    format!("GET /v1/me HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\n\r\n")
        .into_bytes()
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn codex_logs_in_trades_for_a_key_refreshes_and_calls_the_api_with_stand_ins_only() {
    let pki = Pki::new();
    let auth = FakeServer::tls(pki.server_config(AUTH, Flaw::None), auth_handler()).await;
    let api = FakeServer::tls(
        pki.server_config(API, Flaw::None),
        Arc::new(|_| Reply::ok("hello")),
    )
    .await;
    let chatgpt = FakeServer::tls(
        pki.server_config(CHATGPT, Flaw::None),
        Arc::new(|_| Reply::ok("hello")),
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
        .bound(vec![AUTH, API, CHATGPT])
        .allow(vec![AUTH, API, CHATGPT])
        .name(AUTH, auth.addr)
        .name(API, api.addr)
        .name(CHATGPT, chatgpt.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins))
        .build();
    let mut guest = rig.guest();

    // The code exchange: a form in, JSON out.
    let mut client = guest.tls(&format!("{AUTH}:443"), None).await.unwrap();
    client
        .send(&post(
            AUTH,
            "application/x-www-form-urlencoded",
            "grant_type=authorization_code&code=abc&client_id=app",
        ))
        .await;
    let response = client.response("POST").await;
    assert_eq!(response.status, 200);
    let text = String::from_utf8(response.body.clone()).unwrap();
    for real in [real_id(), real_access(), REFRESH.to_owned()] {
        assert!(!text.contains(&real), "a real token reached the guest");
    }
    let login: Value = serde_json::from_str(&text).unwrap();
    let (id, access, refresh) = (
        login["id_token"].as_str().unwrap().to_owned(),
        login["access_token"].as_str().unwrap().to_owned(),
        login["refresh_token"].as_str().unwrap().to_owned(),
    );
    assert_eq!(id.split('.').nth(1), real_id().split('.').nth(1));
    assert!(refresh.starts_with("rt_"));

    // The token exchange: the identity token's stand-in goes out as the real one, and the answer is
    // an API key behind a stand-in of its own.
    client
        .send(&post(
            AUTH,
            "application/x-www-form-urlencoded",
            &format!(
                "grant_type=urn:ietf:params:oauth:grant-type:token-exchange&requested_token=openai-api-key&subject_token={id}"
            ),
        ))
        .await;
    let response = client.response("POST").await;
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let exchanged: Value = serde_json::from_slice(&response.body).unwrap();
    let key = exchanged["access_token"].as_str().unwrap().to_owned();
    assert_ne!(key, API_KEY);
    assert!(key.starts_with("sk-"));
    assert!(!String::from_utf8_lossy(&response.body).contains(API_KEY));

    // The refresh: JSON, the refresh token's stand-in swapped.
    client
        .send(&post(
            AUTH,
            "application/json",
            &json!({"client_id": "app", "grant_type": "refresh_token", "refresh_token": refresh})
                .to_string(),
        ))
        .await;
    assert_eq!(client.response("POST").await.status, 200);
    client.close().await;

    // The API host gets the real key and the real access token, the ChatGPT host the access token.
    let mut client = guest.tls(&format!("{API}:443"), None).await.unwrap();
    client.send(&get(API, &key)).await;
    assert_eq!(client.response("GET").await.status, 200);
    assert_eq!(
        api.recorded().last().unwrap().header("authorization"),
        Some(format!("Bearer {API_KEY}").as_str())
    );
    // The identity token is for the token host only.
    client.send(&get(API, &id)).await;
    assert_eq!(client.response("GET").await.status, 200);
    assert_eq!(
        api.recorded().last().unwrap().header("authorization"),
        Some(format!("Bearer {id}").as_str())
    );
    client.close().await;
    let mut client = guest.tls(&format!("{CHATGPT}:443"), None).await.unwrap();
    client.send(&get(CHATGPT, &access)).await;
    assert_eq!(client.response("GET").await.status, 200);
    assert_eq!(
        chatgpt.recorded().last().unwrap().header("authorization"),
        Some(format!("Bearer {}", real_access()).as_str())
    );
    client.close().await;
    assert!(notices.0.lock().unwrap().is_empty());
}
