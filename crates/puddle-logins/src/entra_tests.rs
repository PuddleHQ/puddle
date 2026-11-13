// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests of the Microsoft Entra profile ([`ENTRA`]) with stand-ins for its tokens: access tokens
//! for several resources at once, each over a thousand characters, an opaque refresh token and an
//! `id_token`. No token here is a real one; every value is built by the test.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use puddle_proxy::{AnswerHead, ExchangeRewriter, SecretValue, StandIns};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};

use crate::profile::{
    Audience, ENTRA, Endpoint, Field, Keep, Method, Role, Scope, Slot, StandInKind,
};
use crate::vault::{Vault, slot_id};
use crate::widened_tests::host;
use crate::{LoginNotices, Logins, Marker, Problem, Profile, builtin};

const LOGIN: &str = "login.microsoftonline.com";

/// The other names of the public cloud's login host. Each answered a token request with Entra's
/// own error when tried against the live service.
const ALIASES: [&str; 3] = [
    "login.windows.net",
    "login.microsoft.com",
    "sts.windows.net",
];

#[derive(Default)]
struct Notices(Mutex<Vec<Problem>>);

impl LoginNotices for Notices {
    fn problem(&self, _: &WorkspaceId, _: &Profile, problem: Problem) {
        self.0.lock().unwrap().push(problem);
    }
}

struct Rig {
    logins: Logins,
    registry: Arc<StandIns>,
    store: Arc<MemoryStore>,
    notices: Arc<Notices>,
}

fn rig() -> Rig {
    let registry = Arc::new(StandIns::new());
    let store = Arc::new(MemoryStore::new());
    let notices = Arc::new(Notices::default());
    let logins = Logins::new(
        WorkspaceId::new("alpha").unwrap(),
        true,
        builtin(),
        Arc::clone(&registry),
        Arc::clone(&store) as Arc<dyn SecretStore>,
        Arc::clone(&notices) as Arc<dyn LoginNotices>,
        Marker::none(),
    );
    Rig {
        logins,
        registry,
        store,
        notices,
    }
}

/// A JWT shaped like an Entra access token: the claims a tool reads, a padding claim that brings it
/// to about `size` characters (real ones carry roles, groups and device ids), and a signature of
/// the length a real one has.
fn jwt(aud: &str, exp: u64, size: usize, signature: char) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"typ":"JWT","alg":"RS256","kid":"test-key"}"#);
    let signature = signature.to_string().repeat(342);
    let bare =
        json!({"aud": aud, "tid": "tenant-1", "upn": "me@example.invalid", "exp": exp, "pad": ""});
    let used = header.len() + signature.len() + 2 + URL_SAFE_NO_PAD.encode(bare.to_string()).len();
    // The payload's base64 is 4/3 of its text.
    let pad = "p".repeat(size.saturating_sub(used) * 3 / 4);
    let claims =
        json!({"aud": aud, "tid": "tenant-1", "upn": "me@example.invalid", "exp": exp, "pad": pad});
    format!(
        "{header}.{}.{signature}",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

fn claims_of(token: &str) -> Value {
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
}

/// A refresh token as Entra writes one: `1.`, then a long run of base64url.
fn refresh(letter: char) -> String {
    format!("1.A{}", letter.to_string().repeat(1400))
}

fn id_token(exp: u64) -> String {
    jwt("client-id", exp, 900, 'I')
}

async fn answer(rig: &Rig, path: &str, body: &Value) -> Option<Value> {
    answer_at(rig, LOGIN, path, body).await
}

async fn answer_at(rig: &Rig, login: &str, path: &str, body: &Value) -> Option<Value> {
    let mut exchange = rig
        .logins
        .begin(&host(login), "POST", path)
        .expect("a token request of the profile");
    let bytes = serde_json::to_vec(body).unwrap();
    let head = AnswerHead {
        status: 200,
        content_type: Some("application/json; charset=utf-8"),
        content_encoding: None,
        content_length: Some(bytes.len() as u64),
    };
    let answer = exchange.answer_mut().unwrap();
    assert!(answer.wants(&head));
    let out = answer.rewrite(&head, &bytes).await?;
    Some(serde_json::from_slice(&out).unwrap())
}

fn v2(access: &str, scope: &str) -> Value {
    json!({
        "token_type": "Bearer", "scope": scope, "expires_in": 3599, "ext_expires_in": 3599,
        "access_token": access, "refresh_token": refresh('R'), "id_token": id_token(1_893_456_000),
        "client_info": "eyJ1aWQiOiJ1In0",
    })
}

async fn reply(rig: &Rig, real: &str, scope: &str) -> Option<Value> {
    answer(rig, "/contoso/oauth2/v2.0/token", &v2(real, scope)).await
}

fn registered(rig: &Rig, stand_in: &str, real: &str) -> bool {
    rig.registry.set_real(stand_in, SecretValue::new(real)) == Ok(true)
}

fn kept(rig: &Rig, slot: Slot) -> (String, String) {
    let saved = Vault::new(
        Arc::clone(&rig.store) as Arc<dyn SecretStore>,
        &WorkspaceId::new("alpha").unwrap(),
    )
    .read(&ENTRA, slot)
    .unwrap()
    .unwrap();
    (saved.stand_in.clone(), saved.real.as_str().to_owned())
}

fn slot(resource: &'static str) -> Slot {
    Slot {
        role: Role::Access,
        resource: Some(resource),
    }
}

#[tokio::test]
async fn one_login_gives_each_resource_its_own_stand_in_that_reads_like_the_token() {
    let rig = rig();
    let arm = jwt(
        "https://management.core.windows.net/",
        1_893_456_000,
        1700,
        'A',
    );
    let graph = jwt(
        "00000003-0000-0000-c000-000000000000",
        1_893_456_100,
        2400,
        'G',
    );
    let devops = jwt(
        "499b84ac-1321-427f-aa17-267ca6975798",
        1_893_456_200,
        1300,
        'D',
    );
    let vault = jwt("https://vault.azure.net", 1_893_456_300, 1500, 'V');
    for real in [&arm, &graph, &devops, &vault] {
        assert!(real.len() > 1024, "a real token is over the old limit");
    }
    let given_arm = reply(
        &rig,
        &arm,
        "https://management.core.windows.net//user_impersonation",
    )
    .await
    .unwrap();
    let given_graph = reply(&rig, &graph, "User.Read profile openid email")
        .await
        .unwrap();
    let given_devops = reply(
        &rig,
        &devops,
        "499b84ac-1321-427f-aa17-267ca6975798/user_impersonation",
    )
    .await
    .unwrap();
    let given_vault = reply(&rig, &vault, "https://vault.azure.net/user_impersonation")
        .await
        .unwrap();

    for (given, real, resource) in [
        (&given_arm, &arm, "arm"),
        (&given_graph, &graph, "graph"),
        (&given_devops, &devops, "devops"),
        (&given_vault, &vault, "vault"),
    ] {
        let stand_in = given["access_token"].as_str().unwrap();
        assert_ne!(stand_in, real);
        assert_eq!(stand_in.len(), real.len());
        assert_eq!(claims_of(stand_in), claims_of(real));
        assert!(!stand_in.contains(real.rsplit('.').next().unwrap()));
        assert!(registered(&rig, stand_in, real), "{resource}");
        assert_eq!(
            kept(&rig, slot(resource)),
            (stand_in.to_owned(), real.clone())
        );
    }
    // The answer's other fields are the service's own.
    assert_eq!(given_arm["expires_in"], 3599);
    assert_eq!(given_arm["client_info"], "eyJ1aWQiOiJ1In0");
    assert!(rig.notices.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_refresh_of_one_resource_leaves_the_stand_in_of_another_working() {
    let rig = rig();
    let graph = jwt("https://graph.microsoft.com", 1_893_456_000, 1800, 'G');
    let given_graph = answer(&rig, "/contoso/oauth2/v2.0/token", &v2(&graph, "User.Read"))
        .await
        .unwrap();
    let graph_stand_in = given_graph["access_token"].as_str().unwrap().to_owned();
    for n in 0..5_u64 {
        let arm = jwt("https://management.azure.com", 1_893_460_000 + n, 1700, 'A');
        answer(
            &rig,
            "/contoso/oauth2/v2.0/token",
            &v2(&arm, "https://management.azure.com/user_impersonation"),
        )
        .await
        .unwrap();
    }
    assert!(registered(&rig, &graph_stand_in, &graph));
    assert_eq!(kept(&rig, slot("graph")).1, graph);
}

#[tokio::test]
async fn the_resource_comes_from_the_answer_or_the_token_whichever_names_it() {
    let rig = rig();
    // The v1 endpoint says it in `resource`, and a token that is not a JWT has no `aud` to read.
    let opaque = format!("EwB{}", "o".repeat(1400));
    let mut body = v2(&opaque, "");
    body["resource"] = json!("https://management.core.windows.net/");
    body.as_object_mut().unwrap().remove("scope");
    let given = answer(&rig, "/contoso/oauth2/token", &body).await.unwrap();
    let stand_in = given["access_token"].as_str().unwrap();
    assert_eq!(
        (stand_in.len(), stand_in.starts_with("Ew")),
        (opaque.len(), true)
    );
    assert_ne!(stand_in, opaque);
    assert_eq!(kept(&rig, slot("arm")).1, opaque);
}

#[tokio::test]
async fn a_resource_the_profile_does_not_name_is_kept_and_its_last_stand_ins_keep_working() {
    let rig = rig();
    let mut given = Vec::new();
    let mut reals = Vec::new();
    for n in 0..5_u64 {
        let real = jwt("https://storage.azure.com", 1_893_456_000 + n, 1500, 'S');
        let reply = answer(
            &rig,
            "/contoso/oauth2/v2.0/token",
            &v2(&real, "https://storage.azure.com/user_impersonation"),
        )
        .await
        .unwrap();
        given.push(reply["access_token"].as_str().unwrap().to_owned());
        reals.push(real);
    }
    assert!(rig.notices.0.lock().unwrap().is_empty());
    assert_eq!(kept(&rig, slot("other")).1, reals[4]);
    for n in 1..5 {
        assert_ne!(given[n], reals[n]);
        assert!(registered(&rig, &given[n], &reals[n]), "{n}");
    }
    assert!(
        !registered(&rig, &given[0], &reals[0]),
        "the oldest is dropped"
    );
}

#[tokio::test]
async fn the_refresh_token_and_the_id_token_are_kept_and_replaced_too() {
    let rig = rig();
    let arm = jwt("https://management.azure.com", 1_893_456_000, 1700, 'A');
    let body = v2(&arm, "https://management.azure.com/user_impersonation");
    let given = answer(&rig, "/contoso/oauth2/v2.0/token", &body)
        .await
        .unwrap();
    let real_refresh = refresh('R');
    let stand_in = given["refresh_token"].as_str().unwrap();
    assert_ne!(stand_in, real_refresh);
    assert_eq!(stand_in.len(), real_refresh.len());
    assert!(stand_in.starts_with("1."));
    assert!(!stand_in.contains("RRRRRRRRRR"));
    assert!(registered(&rig, stand_in, &real_refresh));
    let id = given["id_token"].as_str().unwrap();
    assert_ne!(id, id_token(1_893_456_000));
    assert_eq!(claims_of(id)["upn"], "me@example.invalid");
    // The same login again: the refresh token keeps its stand-in.
    let again = answer(&rig, "/contoso/oauth2/v2.0/token", &body)
        .await
        .unwrap();
    assert_eq!(again["refresh_token"], given["refresh_token"]);
    // Neither is swapped toward an API host.
    for api in [
        "management.azure.com",
        "graph.microsoft.com",
        "dev.azure.com",
    ] {
        let hosts = ENTRA.hosts_for(Scope::TokenEndpoint);
        assert!(!hosts.contains(&host(api)), "{api}");
    }
    // The two, and the access token of a resource the profile does not name.
    assert_eq!(
        ENTRA
            .slots()
            .into_iter()
            .filter(|(_, scope)| *scope == Scope::TokenEndpoint)
            .count(),
        3
    );
}

#[tokio::test]
async fn a_token_over_the_registry_limit_fails_closed_with_a_notice_and_is_not_kept() {
    let rig = rig();
    let huge = jwt("https://management.azure.com", 1_893_456_000, 9000, 'A');
    assert!(huge.len() > 8192);
    let body = v2(&huge, "https://management.azure.com/user_impersonation");
    let mut exchange = rig
        .logins
        .begin(&host(LOGIN), "POST", "/contoso/oauth2/v2.0/token")
        .unwrap();
    let bytes = serde_json::to_vec(&body).unwrap();
    let head = AnswerHead {
        status: 200,
        content_type: Some("application/json"),
        content_encoding: None,
        content_length: Some(bytes.len() as u64),
    };
    let answer = exchange.answer_mut().unwrap();
    assert!(answer.wants(&head));
    assert!(answer.rewrite(&head, &bytes).await.is_none());
    assert_eq!(*rig.notices.0.lock().unwrap(), [Problem::UnusableToken]);
    assert_eq!(rig.registry.len(), 0);
}

#[tokio::test]
async fn forgetting_the_login_removes_every_resources_stand_ins_and_entries() {
    let rig = rig();
    for (aud, scope) in [
        (
            "https://management.azure.com",
            "https://management.azure.com/user_impersonation",
        ),
        ("https://graph.microsoft.com", "User.Read"),
    ] {
        let real = jwt(aud, 1_893_456_000, 1600, 'A');
        answer(&rig, "/contoso/oauth2/v2.0/token", &v2(&real, scope))
            .await
            .unwrap();
    }
    assert!(rig.registry.len() >= 4);
    assert!(rig.logins.forget("entra").await.unwrap());
    assert_eq!(rig.registry.len(), 0);
    for (slot, _) in ENTRA.slots() {
        assert!(
            !Vault::new(
                Arc::clone(&rig.store) as Arc<dyn SecretStore>,
                &WorkspaceId::new("alpha").unwrap()
            )
            .holds(&ENTRA, slot)
            .unwrap(),
            "{slot:?}"
        );
    }
}

#[test]
fn the_entra_profile_pins_its_requests_slots_scopes_and_entry_names() {
    let logins = rig().logins;
    let is_exchange = |m: &str, p: &str| logins.begin(&host(LOGIN), m, p).is_some();
    // Any tenant, either version, POST only, and nothing else on the host.
    for tenant in [
        "contoso.onmicrosoft.com",
        "common",
        "organizations",
        "72f988bf-86f1-41af-91ab-2d7cd011db47",
    ] {
        assert!(
            is_exchange("POST", &format!("/{tenant}/oauth2/v2.0/token")),
            "{tenant}"
        );
        assert!(
            is_exchange("POST", &format!("/{tenant}/oauth2/token")),
            "{tenant}"
        );
        assert!(
            !is_exchange("GET", &format!("/{tenant}/oauth2/v2.0/token")),
            "{tenant}"
        );
        for other in ["devicecode", "authorize", "logout"] {
            assert!(
                !is_exchange("POST", &format!("/{tenant}/oauth2/v2.0/{other}")),
                "{other}"
            );
        }
    }
    assert!(!is_exchange("POST", "/oauth2/v2.0/token"));
    assert!(!is_exchange("POST", "/a/b/oauth2/v2.0/token"));
    // The public cloud's other login hosts answer the same requests, and are read the same way.
    for alias in ALIASES {
        for path in ["/common/oauth2/v2.0/token", "/contoso/oauth2/token"] {
            let exchange = logins.begin(&host(alias), "POST", path);
            assert_eq!(
                exchange.map(|e| e.swap_fields().to_vec()),
                Some(vec!["refresh_token".to_owned()]),
                "{alias}{path}"
            );
        }
        assert!(
            logins
                .begin(&host(alias), "POST", "/common/oauth2/v2.0/devicecode")
                .is_none()
        );
    }
    let names: Vec<(String, Scope)> = ENTRA
        .slots()
        .into_iter()
        .map(|(slot, scope)| (ENTRA.slot_name_of(slot), scope))
        .collect();
    assert_eq!(
        names,
        [
            (
                "entra-access-arm".to_owned(),
                Scope::Hosts(&["management.azure.com", "management.core.windows.net"])
            ),
            (
                "entra-access-devops".to_owned(),
                Scope::Hosts(&["dev.azure.com", "*.dev.azure.com", "*.visualstudio.com"])
            ),
            (
                "entra-access-graph".to_owned(),
                Scope::Hosts(&["graph.microsoft.com", "graph.windows.net"])
            ),
            ("entra-access-other".to_owned(), Scope::TokenEndpoint),
            (
                "entra-access-vault".to_owned(),
                Scope::Hosts(&["*.vault.azure.net"])
            ),
            ("entra-refresh".to_owned(), Scope::TokenEndpoint),
            ("entra-id-token".to_owned(), Scope::TokenEndpoint),
        ]
    );
    // One credential-store entry per slot, and each name differs.
    let ids: std::collections::BTreeSet<_> = ENTRA
        .slots()
        .into_iter()
        .map(|(slot, _)| slot_id("alpha", "entra", slot))
        .collect();
    assert_eq!(ids.len(), 7);
    // The hosts the tokens go to.
    let api = ENTRA.hosts();
    for name in [
        LOGIN,
        "management.azure.com",
        "graph.microsoft.com",
        "dev.azure.com",
        "myorg.visualstudio.com",
        "kv1.vault.azure.net",
    ] {
        assert!(api.contains(&host(name)), "{name}");
    }
    assert!(!api.contains(&host("evil.azure.net")));
    assert!(!api.contains(&host("azure.com")));
}

static NAMED_ONLY: [Profile; 1] = [Profile {
    id: "named",
    name: "Named",
    endpoints: &[Endpoint {
        host: "login.named.test",
        method: Method::Post,
        path: "/token",
        request_fields: &[],
        fields: &[Field {
            path: "access_token",
            resource: Some("arm"),
            role: Role::Access,
            prefixes: &[],
            keep_last: 0,
            only_prefixed: false,
            stand_in: StandInKind::Jwt,
            scope: Scope::Api,
            audience: Audience::Named(&["management.azure.com"]),
            keep: Keep::Stored,
        }],
    }],
    api_hosts: &["management.azure.com"],
}];

#[tokio::test]
async fn a_token_that_no_field_of_the_profile_takes_fails_closed_and_is_not_kept() {
    let registry = Arc::new(StandIns::new());
    let notices = Arc::new(Notices::default());
    let logins = Logins::new(
        WorkspaceId::new("alpha").unwrap(),
        true,
        &NAMED_ONLY,
        Arc::clone(&registry),
        Arc::new(MemoryStore::new()) as Arc<dyn SecretStore>,
        Arc::clone(&notices) as Arc<dyn LoginNotices>,
        Marker::none(),
    );
    let real = jwt("https://graph.microsoft.com", 1_893_456_000, 1500, 'G');
    let body = serde_json::to_vec(&json!({"access_token": real})).unwrap();
    let mut exchange = logins
        .begin(&host("login.named.test"), "POST", "/token")
        .unwrap();
    let head = AnswerHead {
        status: 200,
        content_type: Some("application/json"),
        content_encoding: None,
        content_length: Some(body.len() as u64),
    };
    let answer = exchange.answer_mut().unwrap();
    assert!(answer.wants(&head));
    assert!(answer.rewrite(&head, &body).await.is_none());
    assert_eq!(*notices.0.lock().unwrap(), [Problem::UnusableToken]);
    assert_eq!(registry.len(), 0);
}

#[tokio::test]
async fn a_login_through_another_name_of_the_login_host_is_kept_and_its_refresh_swapped_there() {
    for alias in ALIASES {
        let rig = rig();
        let real = jwt("https://management.azure.com", 1_893_456_000, 1500, 'A');
        let body = v2(&real, "https://management.azure.com/user_impersonation");
        let given = answer_at(&rig, alias, "/contoso/oauth2/v2.0/token", &body)
            .await
            .unwrap();
        let text = given.to_string();
        assert!(!text.contains(real.rsplit('.').next().unwrap()), "{alias}");
        assert!(!text.contains(&refresh('R')), "{alias}");
        assert_eq!(kept(&rig, slot("arm")).1, real, "{alias}");
        // The refresh token's stand-in goes back to every name of the login host.
        let refresh_hosts = ENTRA.hosts_for(Scope::TokenEndpoint);
        for login in ALIASES.iter().chain([&LOGIN]) {
            assert!(refresh_hosts.contains(&host(login)), "{login}");
        }
    }
}

#[tokio::test]
async fn each_access_token_is_swapped_toward_its_own_resources_hosts_only() {
    let resources = [
        (
            "https://management.azure.com",
            &["management.azure.com", "management.core.windows.net"][..],
        ),
        (
            "00000003-0000-0000-c000-000000000000",
            &["graph.microsoft.com", "graph.windows.net"][..],
        ),
        // The older Azure AD Graph goes in the same slot as Microsoft Graph.
        (
            "https://graph.windows.net",
            &["graph.microsoft.com", "graph.windows.net"][..],
        ),
        (
            "499b84ac-1321-427f-aa17-267ca6975798",
            &["dev.azure.com", "myorg.visualstudio.com", "x.dev.azure.com"][..],
        ),
        ("https://vault.azure.net", &["kv1.vault.azure.net"][..]),
        // A resource the profile does not name: toward no API at all.
        ("https://storage.azure.com", &[][..]),
    ];
    let every_api = [
        "management.azure.com",
        "management.core.windows.net",
        "graph.microsoft.com",
        "graph.windows.net",
        "dev.azure.com",
        "x.dev.azure.com",
        "myorg.visualstudio.com",
        "kv1.vault.azure.net",
    ];
    for (aud, own) in resources {
        let rig = rig();
        let real = jwt(aud, 1_893_456_000, 1500, 'T');
        let given = reply(&rig, &real, "openid profile").await.unwrap();
        assert!(
            registered(&rig, given["access_token"].as_str().unwrap(), &real),
            "{aud}"
        );
        // The refresh and identity tokens of the same answer go to the login hosts only, so
        // every API host here is the access token's.
        let hosts = rig.registry.hosts();
        for api in every_api {
            assert_eq!(
                hosts.contains(&host(api)),
                own.contains(&api),
                "{aud} toward {api}"
            );
        }
        assert!(hosts.contains(&host(LOGIN)));
    }
}
