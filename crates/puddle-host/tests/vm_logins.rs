// SPDX-License-Identifier: GPL-3.0-or-later
//! The hostile cases of login capture on a real microVM (K on Linux KVM, W on Windows WHP): what a
//! guest that is root can do to the login machinery from inside, with the real proxy, the real
//! agent and a fake token endpoint behind the company proxy.
//!
//! The fake website answers the Claude Code token endpoint the way the service does (an
//! authorization code gives tokens, a refresh with the real refresh token gives new ones, any other
//! refresh token is refused) and records every request, so each test can say what the server was
//! sent. The tokens it hands out all contain `CANARY` and the stand-ins the guest is given never do,
//! so a real token that reaches the guest, or a stand-in that is swapped where it must not be, shows
//! at once. The host's credential store is an in-memory one: no real store is touched.
//!
//! One test for each of the hostile cases HG-32 (a stand-in sent where it is not for, a body that
//! is not a token message, a body over the cap), HG-33 (another spelling of the token endpoint)
//! and HG-34 (a stand-in used from another workspace).
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "VM test: a failed step fails the test"
)]

mod support;
mod vm_support;

use std::sync::Arc;
use std::time::Duration;

use puddle_e2e::lab::{NameProxy, Seen, WebAnswer, WebHost, WebOptions};
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::WorkspaceName;
use serde_json::{Value, json};
use support::{Api, ended, hermetic_options};
use vm_support::{VmHost, guest, start_vm_host_platform, web_internet};

const TOKEN_HOST: &str = "platform.claude.com";
const API_HOST: &str = "api.anthropic.com";
const OTHER_TOKEN_HOST: &str = "github.com";
const SITES: [&str; 3] = [TOKEN_HOST, API_HOST, OTHER_TOKEN_HOST];

const REAL_ACCESS: &str =
    "sk-ant-oat01-CANARY0access0real0AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const REAL_REFRESH: &str =
    "sk-ant-ort01-CANARY0refresh0real0BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const NEWER_ACCESS: &str =
    "sk-ant-oat01-CANARY0access0newer0EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE";
const NEWER_REFRESH: &str =
    "sk-ant-ort01-CANARY0refresh0newer0FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";

const AUTHORIZATION_CODE: &str =
    r#"{"grant_type":"authorization_code","code":"a-code","client_id":"a-client"}"#;

/// The host a request was for (`Host` or HTTP/2's authority, without a port), lower-case.
fn host_of(seen: &Seen) -> String {
    seen.headers_named("host")
        .first()
        .map(|h| h.split(':').next().unwrap_or_default().to_ascii_lowercase())
        .unwrap_or_default()
}

/// The path the way a server reads it: query and parameters dropped, percent escapes decoded, dot
/// segments and empty segments gone, lower-case.
fn read_path(target: &str) -> String {
    let path = target.split(['?', ';']).next().unwrap_or_default();
    let mut bytes = Vec::new();
    let raw = path.as_bytes();
    let mut at = 0;
    while at < raw.len() {
        let hex = |i: usize| raw.get(i).and_then(|b| char::from(*b).to_digit(16));
        if let (b'%', Some(hi), Some(lo)) = (raw[at], hex(at + 1), hex(at + 2)) {
            bytes.push(u8::try_from(hi * 16 + lo).unwrap());
            at += 3;
        } else {
            bytes.push(raw[at]);
            at += 1;
        }
    }
    let decoded = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    let mut segments: Vec<&str> = Vec::new();
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    format!("/{}", segments.join("/"))
}

fn token_answer(access: &str, refresh: &str) -> WebAnswer {
    WebAnswer {
        status: 200,
        content_type: "application/json",
        body: json!({
            "token_type": "Bearer",
            "access_token": access,
            "expires_in": 28800,
            "refresh_token": refresh,
            "scope": "user:inference user:profile",
        })
        .to_string(),
    }
}

fn refusal() -> WebAnswer {
    WebAnswer {
        status: 400,
        content_type: "application/json",
        body: r#"{"error":"invalid_grant"}"#.to_owned(),
    }
}

/// The Claude Code token endpoint, wherever it is spelled to be.
fn token_endpoint(seen: &Seen) -> Option<WebAnswer> {
    if host_of(seen) != TOKEN_HOST || read_path(&seen.target) != "/v1/oauth/token" {
        return None;
    }
    let Ok(body) = serde_json::from_slice::<Value>(&seen.body) else {
        return Some(refusal());
    };
    Some(match body["grant_type"].as_str() {
        Some("authorization_code") => token_answer(REAL_ACCESS, REAL_REFRESH),
        Some("refresh_token") if body["refresh_token"] == REAL_REFRESH => {
            token_answer(NEWER_ACCESS, NEWER_REFRESH)
        }
        _ => refusal(),
    })
}

/// A host over the fake website, with a workspace for each of `tags` that may reach its sites.
struct Rig {
    vm: VmHost,
    web: WebHost,
    /// The company proxy the host's upstream leg goes through: it stops when dropped.
    _proxy: NameProxy,
    workspaces: Vec<WorkspaceName>,
}

async fn rig(tag: &str, workspaces: &[&str]) -> Rig {
    let mut names = SITES.to_vec();
    names.push("site.invalid");
    let internet = web_internet(
        &names,
        WebOptions {
            responder: Some(Arc::new(token_endpoint)),
            ..WebOptions::default()
        },
        hermetic_options(),
    )
    .await;
    let vm = start_vm_host_platform(tag, internet.options, &internet.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut events = api.events().await;
    let mut made = Vec::new();
    for suffix in workspaces {
        let name = format!("{}-{tag}-{suffix}", vm.settings.prefix.as_str());
        let workspace = WorkspaceName::new(&name).unwrap();
        for site in SITES {
            vm.host
                .store()
                .add_rule(&NewRule {
                    scope: Scope::Workspace(workspace.clone()),
                    pattern: Pattern::parse(site).unwrap(),
                    effect: Effect::Allow,
                    expires_at: None,
                    created_by: Actor::Cli,
                })
                .unwrap();
        }
        let reply = api
            .post(
                "/api/workspaces",
                &json!({"name": name, "repositories": [], "memory_mib": 1024}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        let name: &'static str = Box::leak(name.into_boxed_str());
        let end = events.until(ended(name), Duration::from_secs(600)).await;
        assert_eq!(end["step"], "done", "{end}");
        made.push(workspace);
    }
    Rig {
        vm,
        web: internet.web,
        _proxy: internet.proxy,
        workspaces: made,
    }
}

impl Rig {
    async fn run(&self, workspace: &WorkspaceName, script: &str) -> String {
        guest(&self.vm.runtime, workspace, script).await
    }

    /// A POST from the guest of `workspace`; prints the answer, then the status on its own line.
    async fn post(
        &self,
        workspace: &WorkspaceName,
        url: &str,
        content_type: &str,
        body: &str,
    ) -> String {
        self.run(
            workspace,
            &format!(
                "curl -s --path-as-is --max-time 60 -X POST -H 'content-type: {content_type}' \
                 -d '{body}' -w '\\n%{{http_code}}' '{url}'"
            ),
        )
        .await
    }

    /// A login from the guest of `workspace` at `path` of the token host: the tokens it is given.
    async fn login(&self, workspace: &WorkspaceName, path: &str) -> (String, String) {
        let answer = self
            .post(
                workspace,
                &format!("https://{TOKEN_HOST}{path}"),
                "application/json",
                AUTHORIZATION_CODE,
            )
            .await;
        let (text, status) = answer.trim().rsplit_once('\n').unwrap();
        assert_eq!(status, "200", "{path}: {answer}");
        assert!(
            !text.contains("CANARY"),
            "{path}: a real token reached the guest: {text}"
        );
        let given: Value = serde_json::from_str(text).unwrap();
        let (access, refresh) = (
            given["access_token"].as_str().unwrap().to_owned(),
            given["refresh_token"].as_str().unwrap().to_owned(),
        );
        assert!(access.starts_with("sk-ant-oat01-"), "{access}");
        assert!(refresh.starts_with("sk-ant-ort01-"), "{refresh}");
        (access, refresh)
    }

    /// What the fake website saw, for the host `host` only.
    fn seen_for(&self, host: &str) -> Vec<Seen> {
        self.web
            .seen()
            .into_iter()
            .filter(|s| host_of(s) == host)
            .collect()
    }

    /// How many requests carried a real token, in a header or in the body.
    fn with_real_tokens(&self, real: &[&str]) -> usize {
        self.web
            .seen()
            .iter()
            .filter(|s| {
                let text = format!("{:?} {}", s.headers, String::from_utf8_lossy(&s.body));
                real.iter().any(|token| text.contains(token))
            })
            .count()
    }

    async fn finish(self) {
        let down = self.vm.host.shutdown().await;
        assert!(down.sandboxes.all_stopped(), "{down:?}");
    }
}

fn refresh_body(stand_in: &str) -> String {
    json!({"grant_type": "refresh_token", "refresh_token": stand_in}).to_string()
}

/// HG-32: a stand-in in a token request's refresh field toward a host it is not for, a body that
/// is not a token message, and a body over the cap. The only request the real refresh token is in
/// is the honest refresh the test starts with, so the swap is proven to work on the same guest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_logins_hostile_hg32_a_stand_in_goes_only_where_it_is_for_and_only_in_a_token_message() {
    let rig = rig("hg32", &["one"]).await;
    let one = rig.workspaces[0].clone();
    let (_, refresh) = rig.login(&one, "/v1/oauth/token").await;

    // The honest refresh: the server is sent the real refresh token, the guest gets a stand-in.
    let honest = rig
        .post(
            &one,
            &format!("https://{TOKEN_HOST}/v1/oauth/token"),
            "application/json",
            &refresh_body(&refresh),
        )
        .await;
    assert!(honest.trim().ends_with("200"), "{honest}");
    assert!(!honest.contains("CANARY"), "{honest}");
    let sent = rig.seen_for(TOKEN_HOST);
    assert!(
        String::from_utf8_lossy(&sent.last().unwrap().body).contains(REAL_REFRESH),
        "the refresh token was not swapped: {sent:?}"
    );
    assert_eq!(rig.with_real_tokens(&[REAL_REFRESH]), 1);

    // Toward a host it is not for, on the path of another token endpoint and on the same path.
    for url in [
        format!("https://{OTHER_TOKEN_HOST}/login/oauth/access_token"),
        format!("https://{API_HOST}/v1/oauth/token"),
    ] {
        let body = refresh_body(&refresh);
        let reply = rig.post(&one, &url, "application/json", &body).await;
        assert!(reply.trim().ends_with("200"), "{url}: {reply}");
        let host = url.split('/').nth(2).unwrap();
        let last = rig.seen_for(host).pop().unwrap();
        assert_eq!(last.body, body.as_bytes(), "{url} was sent a changed body");
    }

    // A body that is not a token message, or has the field twice: goes up as sent.
    for (content_type, body) in [
        ("text/plain", format!("refresh_token={refresh}")),
        ("application/json", format!("[\"{refresh}\"")),
        (
            "application/json",
            format!(r#"{{"refresh_token":["{refresh}"]}}"#),
        ),
        (
            "application/x-www-form-urlencoded",
            format!("refresh_token={refresh}&refresh_token={refresh}"),
        ),
    ] {
        let before = rig.seen_for(TOKEN_HOST).len();
        rig.post(
            &one,
            &format!("https://{TOKEN_HOST}/v1/oauth/token"),
            content_type,
            &body,
        )
        .await;
        let after = rig.seen_for(TOKEN_HOST);
        assert_eq!(after.len(), before + 1, "{body}");
        assert_eq!(after.last().unwrap().body, body.as_bytes(), "{body}");
    }

    // A body over the cap is refused by Puddle and nothing goes upstream.
    let before = rig.seen_for(TOKEN_HOST).len();
    let refused = rig
        .run(
            &one,
            &format!(
                "{{ printf '{{\"refresh_token\":\"{refresh}\",\"x\":\"'; head -c 70000 /dev/zero | tr '\\0' y; printf '\"}}'; }} > /tmp/big.json; \
                 curl -si --max-time 60 -X POST -H 'content-type: application/json' --data-binary @/tmp/big.json \
                 https://{TOKEN_HOST}/v1/oauth/token"
            ),
        )
        .await;
    assert!(
        refused.starts_with("HTTP/") && refused.contains(" 413"),
        "{refused}"
    );
    assert!(
        refused.to_lowercase().contains("body_too_large"),
        "{refused}"
    );
    assert_eq!(
        rig.seen_for(TOKEN_HOST).len(),
        before,
        "the oversized body went upstream"
    );

    // The real refresh token reached the server once, in the honest refresh, and nowhere else.
    assert_eq!(
        rig.with_real_tokens(&[REAL_REFRESH, REAL_ACCESS, NEWER_REFRESH, NEWER_ACCESS]),
        1
    );
    rig.finish().await;
}

/// HG-33: the guest starts its own login against the token endpoint with another spelling of the
/// path. The fake server reads every one of them as the token endpoint (as the real one would), so
/// the answer holds real tokens, and the guest must be given stand-ins for all of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_logins_hostile_hg33_another_spelling_of_the_token_endpoint_is_still_captured() {
    let rig = rig("hg33", &["one"]).await;
    let one = rig.workspaces[0].clone();
    let mut given = Vec::new();
    for path in [
        "/v1/oauth/token",
        "/v1/oauth/token/",
        "//v1//oauth/token",
        "/v1/./oauth/token",
        "/v1/x/../oauth/token",
        "/V1/OAuth/Token",
        "/v1/oauth/%74oken",
        "/v1/oauth/token;x=1",
        "/v1/oauth/token?grant=authorization_code",
    ] {
        let before = rig.seen_for(TOKEN_HOST).len();
        given.push(rig.login(&one, path).await);
        assert_eq!(
            rig.seen_for(TOKEN_HOST).len(),
            before + 1,
            "{path} did not reach the server"
        );
    }
    // Every spelling gave the same stand-ins: the one slot of the login.
    assert!(given.windows(2).all(|pair| pair[0] == pair[1]), "{given:?}");
    rig.finish().await;
}

/// HG-34: the stand-ins one workspace captured, used from another workspace, are only text there:
/// not swapped in a header and not in a refresh body, so no real token reaches the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_logins_hostile_hg34_a_stand_in_means_nothing_in_another_workspace() {
    let rig = rig("hg34", &["one", "two"]).await;
    let (one, two) = (rig.workspaces[0].clone(), rig.workspaces[1].clone());
    let (access, refresh) = rig.login(&one, "/v1/oauth/token").await;
    let bearer = format!("Bearer {access}");
    let call_api = |workspace: WorkspaceName| {
        let (rig, access) = (&rig, access.clone());
        async move {
            rig.run(
                &workspace,
                &format!(
                    "curl -s -o /dev/null --max-time 60 -H 'authorization: Bearer {access}' \
                     -w '%{{http_code}}' https://{API_HOST}/v1/messages"
                ),
            )
            .await
        }
    };

    // From the workspace that has the login, the server is sent the real access token.
    assert_eq!(call_api(one.clone()).await.trim(), "200");
    let sent = rig.seen_for(API_HOST).pop().unwrap();
    assert_eq!(
        sent.headers_named("authorization"),
        [format!("Bearer {REAL_ACCESS}")]
    );

    // From the other, the stand-in goes out as it is.
    assert_eq!(call_api(two.clone()).await.trim(), "200");
    let sent = rig.seen_for(API_HOST).pop().unwrap();
    assert_eq!(sent.headers_named("authorization"), [bearer.as_str()]);

    // A refresh body with the stand-in is refused by the server, with the stand-in in it.
    let reply = rig
        .post(
            &two,
            &format!("https://{TOKEN_HOST}/v1/oauth/token"),
            "application/json",
            &refresh_body(&refresh),
        )
        .await;
    assert!(reply.trim().ends_with("400"), "{reply}");
    let sent = rig.seen_for(TOKEN_HOST).pop().unwrap();
    assert!(String::from_utf8_lossy(&sent.body).contains(&refresh));
    assert!(!String::from_utf8_lossy(&sent.body).contains(REAL_REFRESH));
    assert_eq!(
        rig.with_real_tokens(&[REAL_REFRESH, NEWER_REFRESH, NEWER_ACCESS]),
        0
    );
    rig.finish().await;
}
