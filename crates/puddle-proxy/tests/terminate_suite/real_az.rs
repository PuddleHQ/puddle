// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real Azure CLI (`az login --use-device-code`) signing in through the proxy and the real login
//! capture, against fake Microsoft servers. No account is involved and nothing leaves the machine.
//!
//! It proves what the tests with hand-written requests cannot: that `az` accepts the stand-ins
//! (an access token for Azure Resource Manager over a thousand characters, a refresh token, an
//! `id_token` whose claims it reads), keeps working with them (a subscription list, a token for
//! another resource by refresh) and never holds a real token. The tool is the user's own install: the
//! test skips when `az` is not on the path, unless `PUDDLE_REQUIRE_LOGIN_TOOLS` is set. Unix only.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "a skipped test says so; helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use puddle_logins::{LoginNotices, Logins, Marker, Problem, Profile, builtin};
use puddle_proxy::{InjectDecision, StandIns};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use terminate_support::{
    FakeServer, Flaw, Handler, Pki, Recorded, Reply, RigBuilder, TestInjector, captured_logs,
};
use tokio::process::Command;

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const USER: &str = "22222222-2222-2222-2222-222222222222";
const CLIENT: &str = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";
const LOGIN: &str = "login.microsoftonline.com";
const ARM: &str = "management.azure.com";
const GRAPH: &str = "graph.microsoft.com";

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

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn jwt(claims: &Value, signature: &str) -> String {
    format!(
        "{}.{}.{signature}",
        URL_SAFE_NO_PAD.encode(r#"{"typ":"JWT","alg":"RS256","kid":"fake"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
    )
}

/// An access token the way Entra writes one: claims a tool can read and a padding claim standing in
/// for roles and groups, which is what makes real ones over a thousand characters.
fn access_token(aud: &str, tag: &str, fill: char) -> String {
    jwt(
        &json!({
            "aud": aud, "iss": format!("https://sts.windows.net/{TENANT}/"), "tid": TENANT,
            "oid": USER, "upn": "me@example.invalid", "exp": 4_102_444_800_u64,
            "nbf": 1_700_000_000, "iat": 1_700_000_000, "tag": tag, "wids": "p".repeat(900),
        }),
        &fill.to_string().repeat(342),
    )
}

fn id_token() -> String {
    jwt(
        &json!({
            "aud": CLIENT, "iss": format!("https://login.microsoftonline.com/{TENANT}/v2.0"),
            "tid": TENANT, "oid": USER, "sub": "sub-1", "ver": "2.0",
            "preferred_username": "me@example.invalid", "name": "Test User",
            "exp": 4_102_444_800_u64, "nbf": 1_700_000_000, "iat": 1_700_000_000,
        }),
        &"I".repeat(342),
    )
}

fn json_reply(status: u16, value: &Value) -> Reply {
    let body = value.to_string();
    Reply::raw(format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json; charset=utf-8\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    ))
}

fn real_refresh() -> String {
    format!("1.ACANARYrealrefresh{}", "r".repeat(1300))
}

fn real_arm() -> String {
    access_token("https://management.core.windows.net/", "arm", 'A')
}

fn real_graph() -> String {
    access_token("https://graph.microsoft.com", "graph", 'G')
}

fn login_host(request: &Recorded) -> Reply {
    let path = request.target.split('?').next().unwrap_or_default();
    let body = String::from_utf8_lossy(&request.body).into_owned();
    let base = format!("https://{LOGIN}/{TENANT}");
    match (request.method.as_str(), path) {
        ("GET", p) if p.ends_with("/.well-known/openid-configuration") => json_reply(
            200,
            &json!({
                "issuer": format!("{base}/v2.0"),
                "token_endpoint": format!("{base}/oauth2/v2.0/token"),
                "authorization_endpoint": format!("{base}/oauth2/v2.0/authorize"),
                "device_authorization_endpoint": format!("{base}/oauth2/v2.0/devicecode"),
            }),
        ),
        ("POST", p) if p.ends_with("/oauth2/v2.0/devicecode") => json_reply(
            200,
            &json!({
                "user_code": "ABCD1234", "device_code": "dc-1", "interval": 1, "expires_in": 600,
                "verification_uri": "https://microsoft.com/devicelogin",
                "message": "To sign in, use a web browser to open https://microsoft.com/devicelogin and enter the code ABCD1234.",
            }),
        ),
        ("POST", p) if p.ends_with("/oauth2/v2.0/token") => {
            // A refresh for another resource gets that resource's token; the sign-in gets ARM's.
            let graph = body.contains("graph.microsoft.com");
            let (access, scope) = if graph {
                (real_graph(), "https://graph.microsoft.com/User.Read")
            } else {
                (
                    real_arm(),
                    "https://management.core.windows.net//user_impersonation",
                )
            };
            json_reply(
                200,
                &json!({
                    "token_type": "Bearer", "scope": scope, "expires_in": 3599,
                    "ext_expires_in": 3599, "access_token": access,
                    "refresh_token": real_refresh(), "id_token": id_token(),
                    "client_info": URL_SAFE_NO_PAD.encode(json!({"uid": USER, "utid": TENANT}).to_string()),
                }),
            )
        }
        _ => json_reply(404, &json!({"error": "not_found"})),
    }
}

fn arm_host(request: &Recorded) -> Reply {
    let path = request.target.split('?').next().unwrap_or_default();
    match path {
        "/tenants" => json_reply(
            200,
            &json!({"value": [{"id": format!("/tenants/{TENANT}"), "tenantId": TENANT,
                "displayName": "Test", "defaultDomain": "example.invalid", "tenantCategory": "Home"}]}),
        ),
        "/subscriptions" => json_reply(
            200,
            &json!({"value": [{"id": "/subscriptions/33333333-3333-3333-3333-333333333333",
                "subscriptionId": "33333333-3333-3333-3333-333333333333", "tenantId": TENANT,
                "displayName": "Test subscription", "state": "Enabled",
                "managedByTenants": [], "authorizationSource": "RoleBased"}]}),
        ),
        _ => json_reply(404, &json!({"error": {"code": "NotFound"}})),
    }
}

struct Az {
    home: tempfile::TempDir,
    port: u16,
    servers: Vec<(&'static str, FakeServer)>,
    notices: Arc<Notices>,
    program: PathBuf,
    _rig: terminate_support::Rig,
    _guest: terminate_support::Guest,
    _bridge: tokio::task::JoinHandle<()>,
}

impl Az {
    async fn new(program: PathBuf) -> Self {
        let pki = Pki::new();
        let mut rig = RigBuilder::new(&pki);
        let mut servers = Vec::new();
        let mut bound = Vec::new();
        for host in [LOGIN, ARM, GRAPH] {
            let handler: Handler = match host {
                LOGIN => Arc::new(|r: &Recorded| login_host(r)),
                ARM => Arc::new(|r: &Recorded| arm_host(r)),
                _ => Arc::new(|_: &Recorded| json_reply(200, &json!({"value": []}))),
            };
            let server = FakeServer::tls(pki.server_config(host, Flaw::None), handler).await;
            rig = rig.name(host, server.addr);
            bound.push(host);
            servers.push((host, server));
        }
        let registry = Arc::new(StandIns::new());
        let notices = Arc::new(Notices::default());
        let logins = Logins::new(
            WorkspaceId::new("box").unwrap(),
            true,
            builtin(),
            Arc::clone(&registry),
            Arc::new(MemoryStore::new()) as Arc<dyn SecretStore>,
            Arc::clone(&notices) as Arc<dyn LoginNotices>,
            Marker::none(),
        );
        let rig = rig
            .bound(bound.clone())
            .allow(bound)
            .injector(TestInjector::new(|_| InjectDecision::PassThrough))
            .stand_ins(&registry)
            .exchanges(Arc::new(logins))
            .build();
        let guest = rig.guest();
        let (port, bridge) = guest.proxy_port().await;
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("ca.pem"), rig.ca.certificate().pem()).unwrap();
        Self {
            home,
            port,
            servers,
            notices,
            program,
            _rig: rig,
            _guest: guest,
            _bridge: bridge,
        }
    }

    fn server(&self, host: &str) -> &FakeServer {
        &self
            .servers
            .iter()
            .find(|(name, _)| *name == host)
            .unwrap()
            .1
    }

    fn command(&self, args: &[&str]) -> Command {
        let home = self.home.path();
        let ca = home.join("ca.pem");
        let proxy = format!("http://127.0.0.1:{}", self.port);
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home)
            .env("AZURE_CONFIG_DIR", home.join(".azure"))
            .env("TERM", "dumb")
            .env("HTTPS_PROXY", &proxy)
            .env("https_proxy", &proxy)
            .env("REQUESTS_CA_BUNDLE", &ca)
            .env("SSL_CERT_FILE", &ca)
            .env("AZURE_CORE_INSTANCE_DISCOVERY", "false")
            .env("AZURE_CORE_COLLECT_TELEMETRY", "false")
            .env("AZURE_CORE_LOGIN_EXPERIENCE_V2", "off")
            .env("AZURE_CORE_NO_COLOR", "true")
            .env("AZURE_CORE_ONLY_SHOW_ERRORS", "false")
            .current_dir(home)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    async fn run(&self, args: &[&str]) -> (bool, String) {
        let child = self.command(args).spawn().unwrap();
        match tokio::time::timeout(Duration::from_secs(120), child.wait_with_output()).await {
            Ok(Ok(out)) => (
                out.status.success(),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
            ),
            Ok(Err(err)) => (false, err.to_string()),
            Err(_) => (false, "timed out".to_owned()),
        }
    }

    fn home_text(&self) -> String {
        fn walk(dir: &Path, out: &mut String) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    let _ = write!(out, "\n--- {}\n{text}", path.display());
                }
            }
        }
        let mut out = String::new();
        walk(self.home.path(), &mut out);
        out
    }

    fn requests(&self) -> String {
        self.servers
            .iter()
            .flat_map(|(host, server)| {
                server
                    .recorded()
                    .into_iter()
                    .map(move |r| format!("{host} {} {}", r.method, r.target))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn az_login_by_device_code_works_with_stand_ins_and_holds_no_real_token() {
    let Some(program) = on_path("az") else {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_LOGIN_TOOLS").is_none(),
            "PUDDLE_REQUIRE_LOGIN_TOOLS is set but az was not found"
        );
        eprintln!("skipped: az not found");
        return;
    };
    let az = Az::new(program).await;
    let (ok, out) = az
        .run(&[
            "login",
            "--use-device-code",
            "--tenant",
            TENANT,
            "--output",
            "json",
        ])
        .await;
    assert!(ok, "az login failed: {out}\n{}", az.requests());
    assert!(
        out.contains("33333333-3333-3333-3333-333333333333"),
        "{out}"
    );

    // az keeps stand-ins: an access token whose claims it and we read, a refresh token and the
    // id_token, none of them the real ones.
    let cache = std::fs::read_to_string(az.home.path().join(".azure/msal_token_cache.json"))
        .unwrap_or_else(|err| panic!("no token cache ({err}): {out}"));
    let reals = [real_arm(), real_graph(), real_refresh(), id_token()];
    let home = az.home_text();
    for real in &reals {
        let signature = real.rsplit('.').next().unwrap_or(real);
        assert!(!home.contains(real), "a real token is in the tool's home");
        assert!(!captured_logs().contains(real), "a real token is in a log");
        assert!(
            !home.contains(&signature[..40.min(signature.len())]) || real.starts_with("1."),
            "a signature is in the home"
        );
    }
    assert!(!home.contains(&real_refresh()));
    assert!(cache.contains("\"secret\""), "{cache}");

    // The access token az prints is a stand-in with the real claims and the real length.
    let (ok, shown) = az
        .run(&["account", "get-access-token", "--output", "json"])
        .await;
    assert!(ok, "{shown}\n{}", az.requests());
    let shown: Value = serde_json::from_str(shown.trim()).unwrap();
    let token = shown["accessToken"].as_str().unwrap();
    assert_ne!(token, real_arm());
    assert_eq!(token.len(), real_arm().len());
    assert_eq!(
        token.rsplit_once('.').unwrap().0,
        real_arm().rsplit_once('.').unwrap().0
    );

    // A token for another resource is a refresh at the login host with the real refresh token.
    let (ok, shown) = az
        .run(&[
            "account",
            "get-access-token",
            "--resource",
            "https://graph.microsoft.com",
            "--output",
            "json",
        ])
        .await;
    assert!(ok, "{shown}\n{}", az.requests());
    let graph: Value = serde_json::from_str(shown.trim()).unwrap();
    let graph = graph["accessToken"].as_str().unwrap();
    assert_ne!(graph, real_graph());
    assert_eq!(graph.len(), real_graph().len());
    let refreshes: Vec<String> = az
        .server(LOGIN)
        .recorded()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .filter(|b| b.contains("grant_type=refresh_token"))
        .collect();
    assert!(
        !refreshes.is_empty(),
        "no refresh was made\n{}",
        az.requests()
    );
    for body in &refreshes {
        assert!(body.contains(&real_refresh()), "{body}");
    }
    assert_eq!(*az.notices.0.lock().unwrap(), []);

    // The stand-in az holds reaches the API as the real token.
    let seen: Vec<String> = az
        .server(ARM)
        .recorded()
        .iter()
        .filter_map(|r| r.header("authorization").map(str::to_owned))
        .collect();
    assert!(!seen.is_empty(), "{}", az.requests());
    for header in seen {
        assert_eq!(header, format!("Bearer {}", real_arm()));
    }
}
