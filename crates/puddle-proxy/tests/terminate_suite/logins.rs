// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests of captured logins through the real proxy: a login made in the workspace ends with the
//! real tokens on the host and stand-ins in the workspace, the stand-ins are swapped back toward
//! the service's hosts and nowhere else, a refresh works with them, and a login that cannot be
//! captured still works. Fake servers stand in for Claude Code's and GitHub's.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use http_body_util::BodyExt as _;
use puddle_logins::{LoginNotices, Logins, Problem, Profile, builtin};
use puddle_proxy::{InjectDecision, MAX_EXCHANGE_BODY, StandIns};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use terminate_support::h2_rig::{H2Server, Reply as H2Reply, Script, full};
use terminate_support::{
    Client, FakeServer, Flaw, Handler, Pki, Recorded, Reply, Rig, RigBuilder, TestInjector,
    captured_logs,
};

pub(crate) const TOKEN_HOST: &str = "platform.claude.com";
const API_HOST: &str = "api.anthropic.com";

pub(crate) const ACCESS_1: &str =
    "sk-ant-oat01-CANARY-access-one-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(crate) const REFRESH_1: &str =
    "sk-ant-ort01-CANARY-refresh-one-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const ACCESS_2: &str =
    "sk-ant-oat01-CANARY-access-two-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const REFRESH_2: &str =
    "sk-ant-ort01-CANARY-refresh-two-dddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const API_KEY: &str = "sk-ant-api03-CANARY-key-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-the-last-twenty";

const GH_TOKEN: &str = "gho_CANARYgithub0123456789abcdefghijkl";

const REALS: [&str; 6] = [ACCESS_1, REFRESH_1, ACCESS_2, REFRESH_2, API_KEY, GH_TOKEN];

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

fn json_reply(value: &Value) -> Reply {
    let body = value.to_string();
    Reply::raw(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    ))
}

fn form_reply(body: &str) -> Reply {
    Reply::raw(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/x-www-form-urlencoded; charset=utf-8\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    ))
}

fn error_reply(status: u16) -> Reply {
    Reply::status(
        status,
        "content-type: application/json\r\n",
        r#"{"error":"invalid_grant"}"#,
    )
}

fn tokens(access: &str, refresh: &str) -> Value {
    json!({
        "token_type": "Bearer",
        "access_token": access,
        "expires_in": 28800,
        "refresh_token": refresh,
        "scope": "user:inference user:profile",
        "account": {"uuid": "u-1"},
    })
}

/// Claude Code's token endpoint: a code exchange gets the first tokens, a refresh with the first
/// refresh token gets the second, anything else is an error.
fn claude_token_server() -> Handler {
    Arc::new(|request: &Recorded| {
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
            return error_reply(400);
        };
        match body.get("grant_type").and_then(Value::as_str) {
            Some("authorization_code") => json_reply(&tokens(ACCESS_1, REFRESH_1)),
            Some("refresh_token")
                if body.get("refresh_token").and_then(Value::as_str) == Some(REFRESH_1) =>
            {
                json_reply(&tokens(ACCESS_2, REFRESH_2))
            }
            _ => error_reply(400),
        }
    })
}

pub(crate) struct Login {
    pub(crate) rig: Rig,
    logins: Logins,
    registry: Arc<StandIns>,
    store: Arc<MemoryStore>,
    notices: Arc<Notices>,
    token: FakeServer,
    api: FakeServer,
}

fn workspace() -> WorkspaceId {
    WorkspaceId::new("box").unwrap()
}

async fn server(pki: &Pki, name: &str, handler: Handler) -> FakeServer {
    FakeServer::tls(pki.server_config(name, Flaw::None), handler).await
}

fn logins_on(
    registry: &Arc<StandIns>,
    store: &Arc<MemoryStore>,
    enabled: bool,
) -> (Logins, Arc<Notices>) {
    let notices = Arc::new(Notices::default());
    let logins = Logins::new(
        workspace(),
        enabled,
        builtin(),
        Arc::clone(registry),
        Arc::clone(store) as Arc<dyn SecretStore>,
        Arc::clone(&notices) as Arc<dyn LoginNotices>,
        puddle_logins::Marker::none(),
    );
    (logins, notices)
}

pub(crate) async fn claude_login(pki: &Pki, enabled: bool, store: Arc<MemoryStore>) -> Login {
    claude_login_answering(pki, enabled, store, claude_token_server()).await
}

async fn claude_login_answering(
    pki: &Pki,
    enabled: bool,
    store: Arc<MemoryStore>,
    token_handler: Handler,
) -> Login {
    let token = server(pki, TOKEN_HOST, token_handler).await;
    let api = server(pki, API_HOST, Arc::new(|_| Reply::ok("hello"))).await;
    let registry = Arc::new(StandIns::new());
    let (logins, notices) = logins_on(&registry, &store, enabled);
    logins.load().await;
    let rig = RigBuilder::new(pki)
        .bound(vec![TOKEN_HOST, API_HOST, "elsewhere.test"])
        .allow(vec![TOKEN_HOST, API_HOST, "elsewhere.test"])
        .name(TOKEN_HOST, token.addr)
        .name(API_HOST, api.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins.clone()))
        .build();
    Login {
        rig,
        logins,
        registry,
        store,
        notices,
        token,
        api,
    }
}

pub(crate) fn request(method: &str, host: &str, path: &str, extra: &str, body: &str) -> Vec<u8> {
    format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

pub(crate) fn authorization_code() -> String {
    json!({"grant_type": "authorization_code", "code": "fakecode", "code_verifier": "v", "state": "s"})
        .to_string()
}

fn refresh_with(token: &str) -> String {
    json!({"grant_type": "refresh_token", "refresh_token": token, "client_id": "9d1c"}).to_string()
}

/// A request that should carry a token to the API host, and the `Authorization` it arrived with.
pub(crate) async fn api_call(login: &Login, bearer: &str) -> Option<String> {
    let mut guest = login.rig.guest();
    let mut client = guest.tls(&format!("{API_HOST}:443"), None).await.unwrap();
    client
        .send(&request(
            "GET",
            API_HOST,
            "/v1/messages",
            &format!("Authorization: Bearer {bearer}\r\n"),
            "",
        ))
        .await;
    assert_eq!(client.response("GET").await.status, 200);
    let seen = login.api.recorded();
    seen.last()
        .unwrap()
        .header("authorization")
        .map(str::to_owned)
}

fn assert_no_real_token(text: &str) {
    for real in REALS {
        assert!(!text.contains(real), "{real} leaked: {text}");
    }
    assert!(!text.contains("CANARY"), "a canary leaked: {text}");
}

#[tokio::test]
async fn a_claude_login_keeps_the_real_tokens_and_every_later_request_uses_them_through_the_stand_ins()
 {
    let pki = Pki::new();
    let login = claude_login(&pki, true, Arc::new(MemoryStore::new())).await;
    let mut guest = login.rig.guest();
    let mut client = guest.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();

    // The code exchange.
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\nAccept-Encoding: gzip\r\n",
            &authorization_code(),
        ))
        .await;
    let response = client.response("POST").await;
    assert_eq!(response.status, 200);
    assert_no_real_token(&response.text());
    let given: Value = serde_json::from_slice(&response.body).unwrap();
    let access = given["access_token"].as_str().unwrap().to_owned();
    let refresh = given["refresh_token"].as_str().unwrap().to_owned();
    assert!(access.starts_with("sk-ant-oat01-") && access.len() == ACCESS_1.len());
    assert!(refresh.starts_with("sk-ant-ort01-") && refresh.len() == REFRESH_1.len());
    assert_eq!(
        given["expires_in"], 28800,
        "the rest of the answer is the service's"
    );
    assert_eq!(given["scope"], "user:inference user:profile");
    assert_eq!(
        response.header("content-length"),
        Some(response.body.len().to_string().as_str())
    );
    assert_eq!(login.registry.len(), 2);
    assert_eq!(login.token.recorded()[0].header("accept-encoding"), None);

    // The workspace's API call: the real token goes out, the stand-in does not.
    assert_eq!(
        api_call(&login, &access).await.as_deref(),
        Some(format!("Bearer {ACCESS_1}").as_str())
    );

    // A refresh with the stand-in: the real refresh token reaches the service, the answer is
    // captured behind the same stand-ins, and the API call now uses the new real token.
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\n",
            &refresh_with(&refresh),
        ))
        .await;
    let response = client.response("POST").await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_no_real_token(&response.text());
    let again: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(again["access_token"], access.as_str());
    assert_eq!(again["refresh_token"], refresh.as_str());
    let sent: Value = serde_json::from_slice(&login.token.recorded()[1].body).unwrap();
    assert_eq!(sent["refresh_token"], REFRESH_1);
    assert_eq!(sent["client_id"], "9d1c");
    assert_eq!(login.registry.len(), 2);
    assert_eq!(
        api_call(&login, &access).await.as_deref(),
        Some(format!("Bearer {ACCESS_2}").as_str())
    );
    assert!(login.notices.0.lock().unwrap().is_empty());

    // The audit rows and the logs name no token, real or stand-in.
    client.close().await;
    let events = login.rig.events(1).await;
    assert_no_real_token(&format!("{events:?}"));
    let logs = captured_logs();
    assert_no_real_token(&logs);
    assert!(!logs.contains(&access) && !logs.contains(&refresh));
}

#[tokio::test]
async fn a_stand_in_goes_only_toward_its_own_hosts_and_the_refresh_token_only_to_the_token_endpoint()
 {
    let pki = Pki::new();
    let login = claude_login(&pki, true, Arc::new(MemoryStore::new())).await;
    let mut guest = login.rig.guest();
    let mut client = guest.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\n",
            &authorization_code(),
        ))
        .await;
    let given: Value = serde_json::from_slice(&client.response("POST").await.body).unwrap();
    let access = given["access_token"].as_str().unwrap();
    let refresh = given["refresh_token"].as_str().unwrap();

    // The refresh stand-in as a header toward the API host: a decrypted host it is not for.
    assert_eq!(
        api_call(&login, refresh).await.as_deref(),
        Some(format!("Bearer {refresh}").as_str()),
        "unchanged"
    );
    // The access stand-in toward a decrypted host that is not the service's.
    let elsewhere = server(&pki, "elsewhere.test", Arc::new(|_| Reply::ok("x"))).await;
    let rig = RigBuilder::new(&pki)
        .bound(vec![TOKEN_HOST, "elsewhere.test"])
        .allow(vec![TOKEN_HOST, "elsewhere.test"])
        .name("elsewhere.test", elsewhere.addr)
        .name(TOKEN_HOST, login.token.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&login.registry)
        .exchanges(Arc::new(login.logins.clone()))
        .build();
    let mut other = rig.guest();
    let mut away = other.tls("elsewhere.test:443", None).await.unwrap();
    away.send(&request(
        "GET",
        "elsewhere.test",
        "/",
        &format!("Authorization: Bearer {access}\r\n"),
        "",
    ))
    .await;
    assert_eq!(away.response("GET").await.status, 200);
    away.close().await;
    assert_eq!(
        elsewhere.recorded()[0].header("authorization"),
        Some(format!("Bearer {access}").as_str())
    );
    let events = rig.events(1).await;
    assert!(!events[0].unswapped_stand_ins.is_empty() && !events[0].injected);

    // The refresh token in the body of a request to another path of the token host: not an
    // exchange, so nothing is swapped and the service sees the stand-in (and refuses it).
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/other",
            "Content-Type: application/json\r\n",
            &refresh_with(refresh),
        ))
        .await;
    assert_eq!(client.response("POST").await.status, 400);
    let sent = login.token.recorded();
    assert!(String::from_utf8_lossy(&sent.last().unwrap().body).contains(refresh));
    assert_no_real_token(&captured_logs());
}

#[tokio::test]
async fn hostile_hg34_a_captured_stand_in_means_nothing_in_another_workspace() {
    let pki = Pki::new();
    let login = claude_login(&pki, true, Arc::new(MemoryStore::new())).await;
    let mut guest = login.rig.guest();
    let mut client = guest.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\n",
            &authorization_code(),
        ))
        .await;
    let given: Value = serde_json::from_slice(&client.response("POST").await.body).unwrap();
    let access = given["access_token"].as_str().unwrap().to_owned();
    let refresh = given["refresh_token"].as_str().unwrap().to_owned();

    // The second workspace has stand-ins of its own (none) for the same hosts: what the first
    // one captured goes out from there as the text it is, in a header and in a refresh body.
    let mut other = login.rig.other_guest();
    let mut to_api = other.tls(&format!("{API_HOST}:443"), None).await.unwrap();
    to_api
        .send(&request(
            "GET",
            API_HOST,
            "/v1/messages",
            &format!("Authorization: Bearer {access}\r\n"),
            "",
        ))
        .await;
    assert_eq!(to_api.response("GET").await.status, 200);
    assert_eq!(
        login.api.recorded().last().unwrap().header("authorization"),
        Some(format!("Bearer {access}").as_str())
    );
    let mut to_token = other.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();
    to_token
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\n",
            &refresh_with(&refresh),
        ))
        .await;
    assert_eq!(to_token.response("POST").await.status, 400);
    let sent = login.token.recorded();
    assert!(String::from_utf8_lossy(&sent.last().unwrap().body).contains(&refresh));
    assert_no_real_token(&captured_logs());
}

/// Signs in through the proxy and returns what the tool gets.
async fn sign_in(login: &Login) -> terminate_support::Response {
    let mut guest = login.rig.guest();
    let mut client = guest.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\n",
            &authorization_code(),
        ))
        .await;
    client.response("POST").await
}

/// What a refused login looks like to the tool and to the user: a `502` that names the reason
/// and carries nothing of the real answer, one notice, nothing kept and nothing swapped.
fn assert_refused(login: &Login, response: &terminate_support::Response, problem: Problem) {
    assert_eq!(response.status, 502);
    assert_eq!(response.header("x-puddle-blocked"), Some("login_not_kept"));
    let all = format!("{response:?}{}", String::from_utf8_lossy(&response.body));
    assert_no_real_token(&all);
    assert!(
        String::from_utf8_lossy(&response.body).contains("Claude Code sign-in"),
        "the tool is told which sign-in: {all}"
    );
    assert_eq!(*login.notices.0.lock().unwrap(), [problem]);
    assert_eq!(login.registry.len(), 0);
}

#[tokio::test]
async fn a_login_whose_token_cannot_be_kept_fails_with_a_notice_and_the_real_token_never_arrives() {
    let pki = Pki::new();
    let login = claude_login(&pki, true, Arc::new(MemoryStore::new())).await;
    // The credential store goes away after the workspace started.
    login.store.break_it();
    let response = sign_in(&login).await;
    assert_refused(&login, &response, Problem::StoreUnavailable);
    assert!(
        String::from_utf8_lossy(&response.body).contains("credential store"),
        "the reason is named"
    );
    // The tool has nothing to use: its real token never left the host.
    assert_eq!(login.registry.len(), 0);
}

/// One token answer from the service, as raw HTTP/1.1.
type Answer = Box<dyn Fn() -> Reply + Send + Sync>;

fn answering(answer: Answer) -> Handler {
    Arc::new(move |_: &Recorded| answer())
}

fn raw_answer(head: &str, body: &str) -> Reply {
    Reply::raw(format!(
        "HTTP/1.1 200 OK\r\n{head}content-length: {}\r\n\r\n{body}",
        body.len()
    ))
}

#[tokio::test]
async fn every_answer_capture_cannot_read_fails_the_login_instead_of_passing_the_real_token() {
    let real = tokens(ACCESS_1, REFRESH_1).to_string();
    let json = "content-type: application/json\r\n";
    let padded = format!(
        r#"{{"access_token":"{ACCESS_1}","refresh_token":"{REFRESH_1}","pad":"{}"}}"#,
        "x".repeat(MAX_EXCHANGE_BODY)
    );
    let cases: Vec<(&str, Answer, Problem)> = vec![
        (
            "compressed",
            Box::new({
                let real = real.clone();
                move || raw_answer(&format!("{json}content-encoding: gzip\r\n"), &real)
            }),
            Problem::UnexpectedAnswer,
        ),
        (
            "larger than a token answer, with a length",
            Box::new(move || raw_answer(json, &padded)),
            Problem::UnexpectedAnswer,
        ),
        (
            "another content type",
            Box::new({
                let real = real.clone();
                move || raw_answer("content-type: text/html\r\n", &real)
            }),
            Problem::UnexpectedAnswer,
        ),
        (
            "not JSON though it says so",
            Box::new(move || raw_answer(json, "not json at all")),
            Problem::UnexpectedAnswer,
        ),
        (
            "bound to a key",
            Box::new(move || {
                let mut body = tokens(ACCESS_1, REFRESH_1);
                body["token_type"] = json!("DPoP");
                raw_answer(json, &body.to_string())
            }),
            Problem::BoundToken,
        ),
        (
            "a token a stand-in cannot copy",
            Box::new(move || {
                raw_answer(
                    json,
                    &tokens("sk-ant-oat01-CANARY with spaces in it", REFRESH_1).to_string(),
                )
            }),
            Problem::UnusableToken,
        ),
        (
            "a new token in a field the profile does not name",
            Box::new(move || {
                let mut body = tokens(ACCESS_1, REFRESH_1);
                body["id_token"] =
                    json!("eyJhbGciOiJub25lIn0.eyJzdWIiOiJ4In0.CANARYsignaturesignature");
                raw_answer(json, &body.to_string())
            }),
            Problem::UnnamedToken,
        ),
        (
            "a token nested in an object",
            Box::new(move || {
                let mut body = tokens(ACCESS_1, REFRESH_1);
                body["data"] = json!({"session_token": "CANARY-nested-session-token-0123456789"});
                raw_answer(json, &body.to_string())
            }),
            Problem::UnnamedToken,
        ),
        (
            "a named field the answer repeats",
            Box::new(move || {
                let body = format!(
                    "access_token={ACCESS_1}&access_token={ACCESS_2}&refresh_token={REFRESH_1}"
                );
                raw_answer("content-type: application/x-www-form-urlencoded\r\n", &body)
            }),
            Problem::UnnamedToken,
        ),
    ];
    for (name, answer, problem) in cases {
        let pki = Pki::new();
        let login =
            claude_login_answering(&pki, true, Arc::new(MemoryStore::new()), answering(answer))
                .await;
        let response = sign_in(&login).await;
        eprintln!("case: {name}");
        assert_refused(&login, &response, problem);
    }
}

#[tokio::test]
async fn an_answer_without_a_length_that_turns_out_too_large_fails_the_login() {
    let pki = Pki::new();
    let big = format!(
        r#"{{"access_token":"{ACCESS_1}","refresh_token":"{REFRESH_1}","pad":"{}"}}"#,
        "x".repeat(MAX_EXCHANGE_BODY)
    );
    let chunked = Reply::raw(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{big}\r\n0\r\n\r\n",
        big.len()
    ));
    let login = claude_login_answering(
        &pki,
        true,
        Arc::new(MemoryStore::new()),
        Arc::new(move |_: &Recorded| chunked.clone()),
    )
    .await;
    let response = sign_in(&login).await;
    assert_refused(&login, &response, Problem::UnexpectedAnswer);
}

#[tokio::test]
async fn an_answer_with_lifetimes_in_token_named_fields_still_passes_capture() {
    let pki = Pki::new();
    let login = claude_login_answering(
        &pki,
        true,
        Arc::new(MemoryStore::new()),
        answering(Box::new(|| {
            let mut body = tokens(ACCESS_1, REFRESH_1);
            body["refresh_token_expires_in"] = json!(15_897_600);
            body["token_type"] = json!("Bearer");
            raw_answer("content-type: application/json\r\n", &body.to_string())
        })),
    )
    .await;
    let response = sign_in(&login).await;
    assert_eq!(response.status, 200);
    assert_no_real_token(&String::from_utf8_lossy(&response.body));
    assert!(login.notices.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn with_capture_off_the_real_token_goes_to_the_workspace_and_nothing_is_decrypted_for_it() {
    let pki = Pki::new();
    let login = claude_login(&pki, false, Arc::new(MemoryStore::new())).await;
    assert!(login.logins.hosts().is_empty());
    let mut guest = login.rig.guest();
    let mut client = guest.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\nAccept-Encoding: gzip\r\n",
            &authorization_code(),
        ))
        .await;
    let response = client.response("POST").await;
    let given: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(given["access_token"], ACCESS_1);
    assert_eq!(login.registry.len(), 0);
    assert_eq!(
        login.token.recorded()[0].header("accept-encoding"),
        Some("gzip"),
        "nothing is read, so nothing is changed"
    );
    assert!(
        login
            .store
            .get(&puddle_secrets::StoredId::new("x").unwrap())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_stand_ins_a_workspace_still_holds_after_a_restart_work_again() {
    let pki = Pki::new();
    let first = claude_login(&pki, true, Arc::new(MemoryStore::new())).await;
    let mut guest = first.rig.guest();
    let mut client = guest.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\n",
            &authorization_code(),
        ))
        .await;
    let given: Value = serde_json::from_slice(&client.response("POST").await.body).unwrap();
    let access = given["access_token"].as_str().unwrap().to_owned();

    // A new start: new registry, new rig, the same credential store.
    let second = claude_login(&pki, true, Arc::clone(&first.store)).await;
    assert_eq!(second.registry.len(), 2);
    assert_eq!(
        api_call(&second, &access).await.as_deref(),
        Some(format!("Bearer {ACCESS_1}").as_str())
    );
}

#[tokio::test]
async fn the_api_key_a_login_creates_is_swapped_in_x_api_key_toward_the_api_host() {
    let pki = Pki::new();
    let login = claude_login(&pki, true, Arc::new(MemoryStore::new())).await;
    // The key endpoint answers on the API host.
    let key_api = server(
        &pki,
        API_HOST,
        Arc::new(|request: &Recorded| {
            if request.target == "/api/oauth/claude_cli/create_api_key" {
                json_reply(&json!({"raw_key": API_KEY}))
            } else {
                Reply::ok("hello")
            }
        }),
    )
    .await;
    let rig = RigBuilder::new(&pki)
        .bound(vec![TOKEN_HOST, API_HOST])
        .allow(vec![TOKEN_HOST, API_HOST])
        .name(TOKEN_HOST, login.token.addr)
        .name(API_HOST, key_api.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&login.registry)
        .exchanges(Arc::new(login.logins.clone()))
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls(&format!("{API_HOST}:443"), None).await.unwrap();
    client
        .send(&request(
            "POST",
            API_HOST,
            "/api/oauth/claude_cli/create_api_key",
            "Authorization: Bearer whatever\r\nContent-Type: application/x-www-form-urlencoded\r\n",
            "",
        ))
        .await;
    let response = client.response("POST").await;
    let given: Value = serde_json::from_slice(&response.body).unwrap();
    let key = given["raw_key"].as_str().unwrap().to_owned();
    assert_ne!(key, API_KEY);
    assert!(key.starts_with("sk-ant-api03-"));
    assert!(
        key.ends_with(&API_KEY[API_KEY.len() - 20..]),
        "the end is kept"
    );
    client
        .send(&request(
            "GET",
            API_HOST,
            "/v1/messages",
            &format!("x-api-key: {key}\r\n"),
            "",
        ))
        .await;
    assert_eq!(client.response("GET").await.status, 200);
    let seen = key_api.recorded();
    assert_eq!(seen.last().unwrap().header("x-api-key"), Some(API_KEY));
    assert_no_real_token(&captured_logs());
}

// ------------------------------------------------------------------------------------------
// GitHub: a form answer for `gh`, a JSON answer for Copilot CLI, Git's Basic credentials.

const DEVICE_POLL: &str = "client_id=178c&device_code=d&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code";

/// A proxy in front of GitHub's three host groups, with logins on.
struct GithubRig {
    github: FakeServer,
    git_and_api: FakeServer,
    copilot: FakeServer,
    rig: Rig,
    notices: Arc<Notices>,
    _registry: Arc<StandIns>,
}

async fn github_rig() -> GithubRig {
    let pki = Pki::new();
    // The server answers in the format the request's `Accept` asks for, as GitHub does, and says
    // the user has not approved yet for the first poll of each.
    let polls = Arc::new(Mutex::new(0_usize));
    let handler: Handler = {
        let polls = Arc::clone(&polls);
        Arc::new(move |request: &Recorded| {
            let wants_json = request.header("accept").is_some_and(|a| a.contains("json"));
            let mut n = polls.lock().unwrap();
            *n += 1;
            let pending = *n % 2 == 1;
            match (pending, wants_json) {
                (true, true) => json_reply(&json!({"error": "authorization_pending"})),
                (true, false) => form_reply("error=authorization_pending&interval=5"),
                (false, true) => json_reply(
                    &json!({"access_token": GH_TOKEN, "token_type": "bearer", "scope": "repo,gist"}),
                ),
                (false, false) => form_reply(&format!(
                    "access_token={GH_TOKEN}&scope=repo%2Cgist&token_type=bearer"
                )),
            }
        })
    };
    let github = server(&pki, "github.com", handler).await;
    let git_and_api = server(&pki, "api.github.com", Arc::new(|_| Reply::ok("ok"))).await;
    let copilot = server(
        &pki,
        "api.individual.githubcopilot.com",
        Arc::new(|_| Reply::ok("ok")),
    )
    .await;
    let registry = Arc::new(StandIns::new());
    let (logins, notices) = logins_on(&registry, &Arc::new(MemoryStore::new()), true);
    let hosts = vec![
        "github.com",
        "api.github.com",
        "api.individual.githubcopilot.com",
    ];
    let rig = RigBuilder::new(&pki)
        .bound(hosts.clone())
        .allow(hosts)
        .name("github.com", github.addr)
        .name("api.github.com", git_and_api.addr)
        .name("api.individual.githubcopilot.com", copilot.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins))
        .build();
    GithubRig {
        github,
        git_and_api,
        copilot,
        rig,
        notices,
        _registry: registry,
    }
}

impl GithubRig {
    /// gh: no `Accept`, a form answer; the first poll is "pending" and says nothing. Returns the
    /// stand-in the workspace was given.
    async fn gh_form_login(&self, client: &mut Client) -> String {
        for _ in 0..2 {
            client
                .send(&request(
                    "POST",
                    "github.com",
                    "/login/oauth/access_token",
                    "Content-Type: application/x-www-form-urlencoded\r\n",
                    DEVICE_POLL,
                ))
                .await;
        }
        let pending = client.response("POST").await;
        assert_eq!(pending.text(), "error=authorization_pending&interval=5");
        assert!(self.notices.0.lock().unwrap().is_empty());
        let done = client.response("POST").await;
        let text = done.text();
        assert_no_real_token(&text);
        let (first, rest) = text.split_once('&').unwrap();
        assert_eq!(rest, "scope=repo%2Cgist&token_type=bearer");
        let stand_in = first.strip_prefix("access_token=").unwrap().to_owned();
        assert!(stand_in.starts_with("gho_") && stand_in.len() == GH_TOKEN.len());
        assert!(
            done.header("content-type")
                .unwrap()
                .starts_with("application/x-www-form-urlencoded")
        );
        stand_in
    }
}

#[tokio::test]
async fn a_github_login_in_the_form_format_for_gh_is_captured() {
    let g = github_rig().await;
    let mut guest = g.rig.guest();
    let mut client = guest.tls("github.com:443", None).await.unwrap();

    g.gh_form_login(&mut client).await;

    assert_no_real_token(&captured_logs());
}

#[tokio::test]
async fn a_github_login_in_the_json_format_for_copilot_cli_is_captured_behind_the_same_stand_in() {
    let g = github_rig().await;
    let mut guest = g.rig.guest();
    let mut client = guest.tls("github.com:443", None).await.unwrap();
    let stand_in = g.gh_form_login(&mut client).await;

    // Copilot CLI: asks for JSON and gets JSON, behind the same stand-in.
    for _ in 0..2 {
        client
            .send(&request(
                "POST",
                "github.com",
                "/login/oauth/access_token",
                "Content-Type: application/x-www-form-urlencoded\r\nAccept: application/json\r\n",
                DEVICE_POLL,
            ))
            .await;
    }
    let _pending = client.response("POST").await;
    let json_done: Value = serde_json::from_slice(&client.response("POST").await.body).unwrap();
    assert_eq!(json_done["access_token"], stand_in.as_str());
    assert_eq!(json_done["scope"], "repo,gist");

    assert_no_real_token(&captured_logs());
}

#[tokio::test]
async fn a_captured_github_stand_in_is_swapped_for_the_api_bearer() {
    let g = github_rig().await;
    let mut guest = g.rig.guest();
    let mut client = guest.tls("github.com:443", None).await.unwrap();
    let stand_in = g.gh_form_login(&mut client).await;

    // The stand-in as the API's token.
    let mut api = guest.tls("api.github.com:443", None).await.unwrap();
    api.send(&request(
        "GET",
        "api.github.com",
        "/user",
        &format!("Authorization: token {stand_in}\r\n"),
        "",
    ))
    .await;
    assert_eq!(api.response("GET").await.status, 200);
    assert_eq!(
        g.git_and_api.recorded()[0].header("authorization"),
        Some(format!("token {GH_TOKEN}").as_str())
    );

    assert_no_real_token(&captured_logs());
}

#[tokio::test]
async fn a_captured_github_stand_in_is_swapped_for_gits_basic_password() {
    let g = github_rig().await;
    let mut guest = g.rig.guest();
    let mut client = guest.tls("github.com:443", None).await.unwrap();
    let stand_in = g.gh_form_login(&mut client).await;

    // The stand-in as Git's Basic password.
    let mut gh = guest.tls("github.com:443", None).await.unwrap();
    let basic = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{stand_in}"))
    };
    gh.send(&request(
        "GET",
        "github.com",
        "/acme/web.git/info/refs?service=git-upload-pack",
        &format!("Authorization: Basic {basic}\r\n"),
        "",
    ))
    .await;
    assert_eq!(gh.response("GET").await.status, 200);
    let sent = g
        .github
        .recorded()
        .last()
        .unwrap()
        .header("authorization")
        .unwrap()
        .to_owned();
    let decoded = {
        use base64::Engine as _;
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(sent.strip_prefix("Basic ").unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(decoded, format!("x-access-token:{GH_TOKEN}"));

    assert_no_real_token(&captured_logs());
}

#[tokio::test]
async fn a_captured_github_stand_in_is_swapped_on_a_copilot_host() {
    let g = github_rig().await;
    let mut guest = g.rig.guest();
    let mut client = guest.tls("github.com:443", None).await.unwrap();
    let stand_in = g.gh_form_login(&mut client).await;

    // The stand-in on a Copilot host.
    let mut cp = guest
        .tls("api.individual.githubcopilot.com:443", None)
        .await
        .unwrap();
    cp.send(&request(
        "GET",
        "api.individual.githubcopilot.com",
        "/models",
        &format!("Authorization: Bearer {stand_in}\r\n"),
        "",
    ))
    .await;
    assert_eq!(cp.response("GET").await.status, 200);
    assert_eq!(
        g.copilot.recorded()[0].header("authorization"),
        Some(format!("Bearer {GH_TOKEN}").as_str())
    );

    assert_no_real_token(&captured_logs());
}

// ------------------------------------------------------------------------------------------
// HTTP/2: the same login.

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn over_http2_a_claude_login_is_captured_and_swapped_the_same_way() {
    let pki = Pki::new();
    let token_script: Script = Arc::new(|seen| {
        let body: Value = serde_json::from_slice(&seen.body).unwrap_or_default();
        let value = match body.get("grant_type").and_then(Value::as_str) {
            Some("authorization_code") => tokens(ACCESS_1, REFRESH_1),
            Some("refresh_token")
                if body.get("refresh_token").and_then(Value::as_str) == Some(REFRESH_1) =>
            {
                tokens(ACCESS_2, REFRESH_2)
            }
            _ => json!({"error": "invalid_grant"}),
        };
        let text = value.to_string();
        let reply: H2Reply = http::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(
                http_body_util::Full::new(Bytes::from(text))
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        reply
    });
    let token = H2Server::recording(&pki, TOKEN_HOST, token_script).await;
    let api_script: Script = Arc::new(|_| terminate_support::h2_rig::reply(200, "ok"));
    let api = H2Server::recording(&pki, API_HOST, api_script).await;
    let registry = Arc::new(StandIns::new());
    let (logins, notices) = logins_on(&registry, &Arc::new(MemoryStore::new()), true);
    let rig = RigBuilder::new(&pki)
        .bound(vec![TOKEN_HOST, API_HOST])
        .allow(vec![TOKEN_HOST, API_HOST])
        .name(TOKEN_HOST, token.addr)
        .name(API_HOST, api.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins))
        .build();
    let mut guest = rig.guest();
    let mut client = guest
        .h2(&format!("{TOKEN_HOST}:443"), &[b"h2", b"http/1.1"])
        .await;
    let got = client
        .request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            &[
                ("content-type", "application/json"),
                ("accept-encoding", "gzip, br"),
            ],
            full(Bytes::from(authorization_code())),
        )
        .await;
    assert_eq!(got.status, 200);
    assert_no_real_token(&got.text());
    let given: Value = serde_json::from_slice(&got.body).unwrap();
    let access = given["access_token"].as_str().unwrap().to_owned();
    let refresh = given["refresh_token"].as_str().unwrap().to_owned();
    assert_eq!(
        got.header("content-length"),
        Some(got.body.len().to_string().as_str())
    );
    assert_eq!(token.recorded()[0].header("accept-encoding"), None);

    let mut api_client = guest.h2(&format!("{API_HOST}:443"), &[b"h2"]).await;
    let call = api_client
        .request(
            "GET",
            API_HOST,
            "/v1/messages",
            &[("authorization", &format!("Bearer {access}"))],
            full(Bytes::new()),
        )
        .await;
    assert_eq!(call.status, 200);
    assert_eq!(
        api.recorded()[0].header("authorization"),
        Some(format!("Bearer {ACCESS_1}").as_str())
    );

    let refreshed = client
        .request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            &[("content-type", "application/json")],
            full(Bytes::from(refresh_with(&refresh))),
        )
        .await;
    assert_eq!(refreshed.status, 200);
    let again: Value = serde_json::from_slice(&refreshed.body).unwrap();
    assert_eq!(again["access_token"], access.as_str());
    let sent: Value = serde_json::from_slice(&token.recorded()[1].body).unwrap();
    assert_eq!(sent["refresh_token"], REFRESH_1);
    let call = api_client
        .request(
            "GET",
            API_HOST,
            "/v1/messages",
            &[("authorization", &format!("Bearer {access}"))],
            full(Bytes::new()),
        )
        .await;
    assert_eq!(call.status, 200);
    assert_eq!(
        api.recorded()[1].header("authorization"),
        Some(format!("Bearer {ACCESS_2}").as_str())
    );
    assert!(notices.0.lock().unwrap().is_empty());
    assert_no_real_token(&captured_logs());
}

#[tokio::test]
async fn over_http2_a_login_capture_cannot_keep_fails_the_same_way() {
    let cases: [(&str, Option<&str>, Value, Problem); 3] = [
        (
            "store down",
            None,
            tokens(ACCESS_1, REFRESH_1),
            Problem::StoreUnavailable,
        ),
        (
            "compressed",
            Some("gzip"),
            tokens(ACCESS_1, REFRESH_1),
            Problem::UnexpectedAnswer,
        ),
        (
            "unnamed token",
            None,
            {
                let mut body = tokens(ACCESS_1, REFRESH_1);
                body["id_token"] = json!("CANARY-id-token-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                body
            },
            Problem::UnnamedToken,
        ),
    ];
    for (name, encoding, body, problem) in cases {
        let pki = Pki::new();
        let token_script: Script = Arc::new(move |_| {
            let mut builder = http::Response::builder()
                .status(200)
                .header("content-type", "application/json");
            if let Some(encoding) = encoding {
                builder = builder.header("content-encoding", encoding);
            }
            let reply: H2Reply = builder
                .body(
                    http_body_util::Full::new(Bytes::from(body.to_string()))
                        .map_err(|never| match never {})
                        .boxed_unsync(),
                )
                .unwrap();
            reply
        });
        let token = H2Server::recording(&pki, TOKEN_HOST, token_script).await;
        let registry = Arc::new(StandIns::new());
        let store = Arc::new(MemoryStore::new());
        let (logins, notices) = logins_on(&registry, &store, true);
        if name == "store down" {
            store.break_it();
        }
        let rig = RigBuilder::new(&pki)
            .bound(vec![TOKEN_HOST])
            .allow(vec![TOKEN_HOST])
            .name(TOKEN_HOST, token.addr)
            .injector(TestInjector::new(|_| InjectDecision::PassThrough))
            .stand_ins(&registry)
            .exchanges(Arc::new(logins))
            .build();
        let mut guest = rig.guest();
        let mut client = guest
            .h2(&format!("{TOKEN_HOST}:443"), &[b"h2", b"http/1.1"])
            .await;
        let got = client
            .request(
                "POST",
                TOKEN_HOST,
                "/v1/oauth/token",
                &[("content-type", "application/json")],
                full(Bytes::from(authorization_code())),
            )
            .await;
        eprintln!("case: {name}");
        assert_eq!(got.status, 502);
        assert_eq!(got.header("x-puddle-blocked"), Some("login_not_kept"));
        assert_no_real_token(&got.text());
        assert_eq!(*notices.0.lock().unwrap(), [problem]);
        assert_eq!(registry.len(), 0);
    }
}

/// A workspace with Claude's login (capture `enabled` or not) and one more allowed host,
/// `spliced.test`, that is not decrypted and whose server is returned.
async fn login_beside_a_spliced_host(pki: &Pki, enabled: bool) -> (Rig, FakeServer, FakeServer) {
    let token = server(pki, TOKEN_HOST, claude_token_server()).await;
    let spliced = server(
        pki,
        "spliced.test",
        Arc::new(|_| Reply::ok("the real thing")),
    )
    .await;
    let registry = Arc::new(StandIns::new());
    let (logins, _) = logins_on(&registry, &Arc::new(MemoryStore::new()), enabled);
    logins.load().await;
    let rig = RigBuilder::new(pki)
        .bound(vec![TOKEN_HOST, API_HOST])
        .allow(vec![TOKEN_HOST, API_HOST, "spliced.test"])
        .name(TOKEN_HOST, token.addr)
        .name("spliced.test", spliced.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins))
        .build();
    (rig, token, spliced)
}

/// `CONNECT authority`, then a TLS handshake that trusts the fake internet and asks for `sni`.
async fn handshake_asking_for(
    rig: &Rig,
    pki: &Pki,
    authority: &str,
    sni: &str,
) -> (
    terminate_support::Guest,
    std::io::Result<terminate_support::TlsStream>,
) {
    let mut guest = rig.guest();
    let (code, reader) = guest.connect_to(authority).await;
    assert_eq!(code, 200);
    let config = terminate_support::Guest::client_config(std::slice::from_ref(&pki.root), &[]);
    let result = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(
            rustls::pki_types::ServerName::try_from(sni.to_owned()).unwrap(),
            reader,
        )
        .await;
    // The guest stays with the caller: dropping it would cut the route under the record.
    (guest, result)
}

#[tokio::test]
async fn a_tunnel_to_another_host_that_asks_for_the_token_endpoint_is_refused() {
    let pki = Pki::new();
    let (rig, token, spliced) = login_beside_a_spliced_host(&pki, true).await;
    // Another name, another port: the proxy does not decrypt either, and the hello names the
    // token endpoint, so no refresh token or code can go to the real server unswapped.
    let mut guests = Vec::new();
    for authority in ["spliced.test:443", "spliced.test:8443"] {
        let (guest, refused) = handshake_asking_for(&rig, &pki, authority, TOKEN_HOST).await;
        assert!(refused.is_err(), "{authority}: the guest gets an alert");
        guests.push(guest);
    }
    let events = rig.events(2).await;
    for event in events {
        assert_eq!(event.reason.to_string(), "token_endpoint_spliced");
        assert_eq!(event.decision, puddle_types::ConnectionDecision::Blocked);
    }
    assert_eq!(token.accepted(), 0);
    assert_eq!(spliced.accepted(), 2, "connected, but never sent the hello");
    assert!(spliced.recorded().is_empty());
}

#[tokio::test]
async fn another_name_in_a_spliced_tunnel_and_the_decrypted_endpoint_itself_still_work() {
    let pki = Pki::new();
    let (rig, token, spliced) = login_beside_a_spliced_host(&pki, true).await;
    // Spliced and honest: the server's own certificate, no refusal.
    let mut other = rig.guest();
    let mut client = other
        .tls_trusting("spliced.test:443", std::slice::from_ref(&pki.root))
        .await
        .unwrap();
    assert_eq!(
        client.get("spliced.test", "/").await.text(),
        "the real thing"
    );
    drop(client);
    // A tunnel to the token endpoint itself is decrypted, as before.
    let mut guest = rig.guest();
    let mut client = guest.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\n",
            &authorization_code(),
        ))
        .await;
    assert_eq!(client.response("POST").await.status, 200);
    assert_eq!(spliced.recorded().len(), 1);
    assert_eq!(token.recorded().len(), 1);
    drop(client);
    for event in rig.events(2).await {
        assert_ne!(event.reason.to_string(), "token_endpoint_spliced");
    }
}

#[tokio::test]
async fn with_capture_off_a_tunnel_that_asks_for_the_token_endpoint_is_left_alone() {
    let pki = Pki::new();
    let (rig, token, _spliced) = login_beside_a_spliced_host(&pki, false).await;
    // Nothing is swapped for this workspace, so there is nothing to protect.
    let (_guest, result) = handshake_asking_for(&rig, &pki, "spliced.test:443", TOKEN_HOST).await;
    assert!(
        result.is_err(),
        "the server's certificate is for another name"
    );
    assert_eq!(rig.events(1).await[0].reason.to_string(), "rule");
    assert_eq!(token.accepted(), 0);
}

#[tokio::test]
async fn a_host_decrypted_only_because_everything_is_asks_for_the_token_endpoint_in_vain() {
    let pki = Pki::new();
    let token = server(&pki, TOKEN_HOST, claude_token_server()).await;
    let free = server(&pki, "free.test", Arc::new(|_| Reply::ok("free"))).await;
    let registry = Arc::new(StandIns::new());
    let (logins, _) = logins_on(&registry, &Arc::new(MemoryStore::new()), true);
    logins.load().await;
    let rig = RigBuilder::new(&pki)
        .bound(vec![TOKEN_HOST, API_HOST])
        .allow(vec![TOKEN_HOST, API_HOST, "free.test"])
        .name(TOKEN_HOST, token.addr)
        .name("free.test", free.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins))
        .decrypting_all(true, &[])
        .build();
    let (_guest, refused) = handshake_asking_for(&rig, &pki, "free.test:443", TOKEN_HOST).await;
    assert!(refused.is_err());
    let event = rig.events(1).await.remove(0);
    assert_eq!(event.reason.to_string(), "token_endpoint_spliced");
    assert_eq!(event.decision, puddle_types::ConnectionDecision::Blocked);
    assert_eq!(free.accepted(), 0);
}
