// SPDX-License-Identifier: AGPL-3.0-or-later
//! A Microsoft Entra login through the real proxy: an `az`-style sign-in gets access tokens for four
//! resources, a refresh token and an `id_token`, each over a thousand characters where Entra's
//! are. The workspace holds stand-ins only; each is swapped for its own real token toward its
//! resource's host (a bearer token for Azure Resource Manager, Graph and Key Vault, and the Basic
//! credentials Git sends to Azure `DevOps`), and the refresh token only toward the login host.
//! Fake servers stand in for Microsoft; every token is built by the test.
#![expect(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use puddle_logins::{LoginNotices, Logins, Marker, Problem, Profile, builtin};
use puddle_proxy::{InjectDecision, StandIns};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use terminate_support::{
    FakeServer, Flaw, Handler, Pki, Recorded, Reply, RigBuilder, TestInjector,
};

const LOGIN: &str = "login.microsoftonline.com";
const ARM: &str = "management.azure.com";
const GRAPH: &str = "graph.microsoft.com";
const DEVOPS: &str = "dev.azure.com";
const VAULT: &str = "kv1.vault.azure.net";

struct Quiet(Mutex<Vec<Problem>>);

impl LoginNotices for Quiet {
    fn problem(&self, _: &WorkspaceId, _: &Profile, problem: Problem) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(problem);
    }
}

fn jwt(aud: &str, exp: u64, size: usize, signature: char) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"typ":"JWT","alg":"RS256"}"#);
    let signature = signature.to_string().repeat(342);
    let pad = "p".repeat(size.saturating_sub(header.len() + signature.len() + 200) * 3 / 4);
    let claims =
        json!({"aud": aud, "tid": "tenant-1", "upn": "me@example.invalid", "exp": exp, "pad": pad});
    format!(
        "{header}.{}.{signature}",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

fn real_refresh(letter: char) -> String {
    format!("1.A{}", letter.to_string().repeat(1400))
}

fn reply(access: &str, scope: &str, refresh: &str) -> Reply {
    let body = json!({
        "token_type": "Bearer", "scope": scope, "expires_in": 3599,
        "access_token": access, "refresh_token": refresh,
        "id_token": jwt("client-id", 1_893_456_000, 900, 'I'),
    })
    .to_string();
    Reply::raw(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json; charset=utf-8\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    ))
}

fn request(method: &str, host: &str, path: &str, extra: &str, body: &str) -> Vec<u8> {
    format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

type Reals = [(&'static str, String, &'static str); 4];

struct World {
    _rig: terminate_support::Rig,
    guest: terminate_support::Guest,
    login: FakeServer,
    resources: Vec<(&'static str, FakeServer)>,
    notices: Arc<Quiet>,
    reals: Reals,
    refresh: String,
}

async fn world() -> World {
    let pki = Pki::new();
    let reals = [
        (
            ARM,
            jwt(
                "https://management.core.windows.net/",
                1_893_456_001,
                1700,
                'A',
            ),
            "https://management.core.windows.net//user_impersonation",
        ),
        (
            GRAPH,
            jwt("https://graph.microsoft.com", 1_893_456_002, 2400, 'G'),
            "User.Read profile openid email",
        ),
        (
            DEVOPS,
            jwt(
                "499b84ac-1321-427f-aa17-267ca6975798",
                1_893_456_003,
                1300,
                'D',
            ),
            "499b84ac-1321-427f-aa17-267ca6975798/user_impersonation",
        ),
        (
            VAULT,
            jwt("https://vault.azure.net", 1_893_456_004, 1500, 'V'),
            "https://vault.azure.net/user_impersonation",
        ),
    ];
    let refresh = real_refresh('R');
    let served = Arc::new(Mutex::new(0_usize));
    let handler: Handler = {
        let (reals, served, refresh) = (reals.clone(), Arc::clone(&served), refresh.clone());
        Arc::new(move |_: &Recorded| {
            let mut n = served.lock().unwrap();
            let (_, access, scope) = &reals[(*n).min(reals.len() - 1)];
            *n += 1;
            reply(access, scope, &refresh)
        })
    };
    let login = FakeServer::tls(pki.server_config(LOGIN, Flaw::None), handler).await;
    let mut resources = Vec::new();
    for host in [ARM, GRAPH, DEVOPS, VAULT] {
        resources.push((
            host,
            FakeServer::tls(
                pki.server_config(host, Flaw::None),
                Arc::new(|_| Reply::ok("hello")),
            )
            .await,
        ));
    }
    let registry = Arc::new(StandIns::new());
    let notices = Arc::new(Quiet(Mutex::new(Vec::new())));
    let logins = Logins::new(
        WorkspaceId::new("box").unwrap(),
        true,
        builtin(),
        Arc::clone(&registry),
        Arc::new(MemoryStore::new()) as Arc<dyn SecretStore>,
        Arc::clone(&notices) as Arc<dyn LoginNotices>,
        Marker::none(),
    );
    let mut rig = RigBuilder::new(&pki)
        .bound(vec![LOGIN, ARM, GRAPH, DEVOPS, VAULT])
        .allow(vec![LOGIN, ARM, GRAPH, DEVOPS, VAULT])
        .name(LOGIN, login.addr);
    for (host, server) in &resources {
        rig = rig.name(host, server.addr);
    }
    let rig = rig
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins))
        .build();
    let guest = rig.guest();
    World {
        _rig: rig,
        guest,
        login,
        resources,
        notices,
        reals,
        refresh,
    }
}

async fn sign_in(w: &mut World) -> Vec<Value> {
    let World {
        guest,
        reals,
        refresh,
        ..
    } = w;

    let _ = &mut *guest;
    // Four sign-ins (one per resource), as MSAL sends them: a form, the tenant in the path.
    let mut given = Vec::new();
    for _ in 0..4 {
        let mut client = guest.tls(&format!("{LOGIN}:443"), None).await.unwrap();
        client
            .send(&request(
                "POST",
                LOGIN,
                "/contoso.onmicrosoft.com/oauth2/v2.0/token",
                "Content-Type: application/x-www-form-urlencoded\r\n",
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&client_id=04b0&device_code=abc",
            ))
            .await;
        let response = client.response("POST").await;
        assert_eq!(response.status, 200);
        let text = response.text();
        assert!(!text.contains(&*refresh) && !text.contains(&"R".repeat(40)));
        for (_, access, _) in reals.iter() {
            assert!(!text.contains(access.rsplit('.').next().unwrap()));
        }
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["expires_in"], 3599);
        given.push(body);
        client.close().await;
    }
    for (body, (_, real, _)) in given.iter().zip(reals.iter()) {
        let stand_in = body["access_token"].as_str().unwrap();
        assert_ne!(stand_in, real);
        assert_eq!(stand_in.len(), real.len());
    }
    let refresh_stand_in = given[3]["refresh_token"].as_str().unwrap().to_owned();
    assert_eq!(refresh_stand_in.len(), refresh.len());
    assert_ne!(refresh_stand_in, *refresh);
    given
}

async fn assert_each_stand_in_swaps_toward_its_resource(w: &mut World, given: &[Value]) {
    let World {
        guest,
        reals,
        resources,
        ..
    } = w;
    // Each stand-in reaches its resource's host as the real token: a bearer value, or the
    // Basic credentials Git sends to Azure DevOps.
    for ((stand_in_body, (host, real, _)), (_, server)) in
        given.iter().zip(reals.iter()).zip(resources.iter())
    {
        let stand_in = stand_in_body["access_token"].as_str().unwrap();
        let mut client = guest.tls(&format!("{host}:443"), None).await.unwrap();
        let (sent, expected) = if *host == DEVOPS {
            (
                format!("Basic {}", STANDARD.encode(format!(":{stand_in}"))),
                format!("Basic {}", STANDARD.encode(format!(":{real}"))),
            )
        } else {
            (format!("Bearer {stand_in}"), format!("Bearer {real}"))
        };
        client
            .send(&request(
                "GET",
                host,
                "/x",
                &format!("Authorization: {sent}\r\n"),
                "",
            ))
            .await;
        assert_eq!(client.response("GET").await.status, 200);
        assert_eq!(
            server.recorded().last().unwrap().header("authorization"),
            Some(expected.as_str()),
            "{host}"
        );
        client.close().await;
    }
}

async fn assert_refresh_and_id_tokens_stay_off_the_resources(w: &mut World, given: &[Value]) {
    let refresh_stand_in = given[3]["refresh_token"].as_str().unwrap().to_owned();
    let id_stand_in = given[3]["id_token"].as_str().unwrap().to_owned();
    let refresh = w.refresh.clone();
    let World {
        guest,
        login,
        resources,
        notices,
        ..
    } = w;
    // The refresh token goes to the login host only, and the id_token is never swapped.
    let mut client = guest.tls(&format!("{ARM}:443"), None).await.unwrap();
    client
        .send(&request(
            "GET",
            ARM,
            "/x",
            &format!("Authorization: Bearer {refresh_stand_in}\r\nX-Id: {id_stand_in}\r\n"),
            "",
        ))
        .await;
    assert_eq!(client.response("GET").await.status, 200);
    let seen = resources[0].1.recorded();
    let seen = seen.last().unwrap();
    assert_eq!(
        seen.header("authorization"),
        Some(format!("Bearer {refresh_stand_in}").as_str())
    );
    assert_eq!(seen.header("x-id"), Some(id_stand_in.as_str()));
    client.close().await;

    let mut client = guest.tls(&format!("{LOGIN}:443"), None).await.unwrap();
    client
        .send(&request(
            "POST",
            LOGIN,
            "/contoso.onmicrosoft.com/oauth2/v2.0/token",
            "Content-Type: application/x-www-form-urlencoded\r\n",
            &format!("grant_type=refresh_token&client_id=04b0&refresh_token={refresh_stand_in}"),
        ))
        .await;
    assert_eq!(client.response("POST").await.status, 200);
    let sent = String::from_utf8(login.recorded().last().unwrap().body.clone()).unwrap();
    assert!(sent.contains(&format!("refresh_token={refresh}")), "{sent}");
    assert!(!sent.contains(&refresh_stand_in));
    assert!(notices.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_entra_login_reaches_each_resource_with_its_own_real_token_and_the_workspace_holds_none()
{
    let mut w = world().await;
    let given = sign_in(&mut w).await;
    assert_each_stand_in_swaps_toward_its_resource(&mut w, &given).await;
    assert_refresh_and_id_tokens_stay_off_the_resources(&mut w, &given).await;
}
