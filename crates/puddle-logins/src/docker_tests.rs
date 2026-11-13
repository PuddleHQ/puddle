// SPDX-License-Identifier: GPL-3.0-or-later
//! The Docker Hub profile: the device flow's token answer (two JWTs and a refresh token), the
//! personal access token `desktop-generate` mints from the access token (a nested field), and the
//! revoke request that carries the refresh token back.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use puddle_proxy::ExchangeRewriter as _;
use puddle_types::WorkspaceId;
use serde_json::{Value, json};

use crate::logins::MEMORY_KEPT;
use crate::profile::{DOCKER, Role, Scope, Slot};
use crate::vault::Vault;
use crate::widened_tests::{Rig, answered, host, rig_for};

static PROFILES: [crate::profile::Profile; 1] = [DOCKER];

const REFRESH: &str = "v1.CANARY0docker0refresh0AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PAT: &str = "dckr_pat_CANARY0docker0pat0AAAAAAA";

fn jwt(claims: &Value) -> String {
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT","kid":"k1"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
        "S".repeat(342),
    )
}

fn part(token: &str, index: usize) -> Value {
    let segment = token.split('.').nth(index).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segment).unwrap()).unwrap()
}

fn access_token() -> String {
    jwt(
        &json!({"exp": 1_893_456_000, "https://hub.docker.com": {"username": "someone", "email": "me@example.test"}}),
    )
}

fn kept(rig: &Rig, role: impl Into<Slot>) -> Option<(String, String)> {
    Vault::new(
        Arc::clone(&rig.store) as _,
        &WorkspaceId::new("alpha").unwrap(),
    )
    .read(&DOCKER, role)
    .unwrap()
    .map(|saved| (saved.stand_in.clone(), saved.real.to_string()))
}

async fn poll(rig: &Rig, access: &str) -> Value {
    answered(
        rig,
        "login.docker.com",
        "POST",
        "/oauth/token",
        &json!({"access_token": access, "id_token": jwt(&json!({"sub": "auth0|1"})), "refresh_token": REFRESH, "scope": "openid offline_access", "expires_in": 86400, "token_type": "Bearer"}),
    )
    .await
}

#[test]
fn the_docker_profile_has_six_stored_slots_and_one_in_memory_with_their_scopes() {
    let names = |slots: Vec<(Slot, Scope)>| -> Vec<(String, Scope)> {
        slots
            .into_iter()
            .map(|(slot, scope)| (DOCKER.slot_name_of(slot), scope))
            .collect()
    };
    assert_eq!(
        names(DOCKER.slots()),
        [
            ("docker-access".to_owned(), Scope::Api),
            ("docker-refresh".to_owned(), Scope::TokenEndpoint),
            ("docker-api-key".to_owned(), Scope::Api),
            ("docker-api-key-created".to_owned(), Scope::Api),
            ("docker-api-key-org".to_owned(), Scope::Api),
            ("docker-id-token".to_owned(), Scope::TokenEndpoint),
        ]
    );
    assert_eq!(
        names(DOCKER.memory_slots()),
        [(
            "docker-access-registry".to_owned(),
            Scope::Hosts(&["registry-1.docker.io"])
        )]
    );
}

#[test]
fn the_access_token_and_the_pat_reach_the_registry_hosts_and_the_refresh_token_only_the_login_host()
{
    let api = DOCKER.hosts_for(Scope::Api);
    for name in [
        "login.docker.com",
        "hub.docker.com",
        "auth.docker.io",
        "registry-1.docker.io",
    ] {
        assert!(api.contains(&host(name)), "{name}");
    }
    assert!(!api.contains(&host("docker.com")));
    assert!(!api.contains(&host("evildocker.io")));
    let token = DOCKER.hosts_for(Scope::TokenEndpoint);
    assert!(token.contains(&host("login.docker.com")));
    for name in ["hub.docker.com", "auth.docker.io", "registry-1.docker.io"] {
        assert!(!token.contains(&host(name)), "{name}");
    }
}

#[test]
fn a_refresh_and_a_revoke_swap_the_refresh_token_and_the_pat_request_swaps_nothing() {
    let rig = rig_for(&PROFILES);
    let refresh = rig
        .logins
        .begin(&host("login.docker.com"), "POST", "/oauth/token")
        .unwrap();
    assert_eq!(refresh.swap_fields(), ["refresh_token"]);
    let mut revoke = rig
        .logins
        .begin(&host("login.docker.com"), "POST", "/oauth/revoke")
        .unwrap();
    assert_eq!(revoke.swap_fields(), ["token"]);
    assert!(
        revoke.answer_mut().is_none(),
        "a revoke's empty answer is not read"
    );
    let mut pat = rig
        .logins
        .begin(
            &host("hub.docker.com"),
            "POST",
            "/v2/access-tokens/desktop-generate",
        )
        .unwrap();
    assert_eq!(pat.swap_fields(), [] as [&str; 0]);
    assert!(pat.answer_mut().is_some());
    // Creating an access token through Docker Hub's API, and asking the registry's token server
    // for a bearer token (`GET`, or `POST` in the `OAuth2` form), are read too; none swaps a field.
    for (host_name, method, path) in [
        ("hub.docker.com", "POST", "/v2/access-tokens"),
        ("hub.docker.com", "POST", "/v2/orgs/acme/access-tokens"),
        ("auth.docker.io", "GET", "/token"),
        ("auth.docker.io", "POST", "/token"),
    ] {
        let mut read = rig
            .logins
            .begin(&host(host_name), method, path)
            .unwrap_or_else(|| panic!("{host_name} {method} {path}"));
        assert_eq!(read.swap_fields(), [] as [&str; 0]);
        assert!(read.answer_mut().is_some(), "{host_name} {path}");
    }
    for (method, path) in [
        ("GET", "/v2/access-tokens/desktop-generate"),
        ("GET", "/v2/access-tokens"),
        ("POST", "/v2/orgs/acme/access-tokens/abc"),
        ("POST", "/v2/access-tokens/desktop-generate/other"),
    ] {
        assert!(
            rig.logins
                .begin(&host("hub.docker.com"), method, path)
                .is_none(),
            "{method} {path}"
        );
    }
}

#[tokio::test]
async fn a_device_flow_answer_keeps_the_claims_docker_reads_and_shapes_the_refresh_token() {
    let rig = rig_for(&PROFILES);
    let access = access_token();
    let given = poll(&rig, &access).await;
    let stand_in = given["access_token"].as_str().unwrap();
    assert_ne!(stand_in, access);
    assert_eq!(stand_in.len(), access.len());
    // `docker` reads the algorithm and the username claim, and verifies nothing.
    assert_eq!(part(stand_in, 0)["alg"], "RS256");
    assert_eq!(part(stand_in, 1), part(&access, 1));
    assert_ne!(stand_in.rsplit('.').next(), access.rsplit('.').next());
    assert_ne!(given["refresh_token"], REFRESH);
    assert_eq!(
        given["refresh_token"].as_str().unwrap().len(),
        REFRESH.len()
    );
    assert_eq!(given["expires_in"], 86400);
    for role in [Role::Access, Role::IdToken, Role::Refresh] {
        assert!(kept(&rig, role).is_some(), "{role:?}");
    }
    assert_eq!(kept(&rig, Role::Access).unwrap().1, access);
    assert!(!given.to_string().contains(REFRESH));
    assert_eq!(rig.registry.len(), 3);
}

#[tokio::test]
async fn the_pat_in_the_nested_data_field_is_replaced_and_the_rest_of_the_answer_is_left() {
    let rig = rig_for(&PROFILES);
    poll(&rig, &access_token()).await;
    let given = answered(
        &rig,
        "hub.docker.com",
        "POST",
        "/v2/access-tokens/desktop-generate",
        &json!({"data": {"uuid": "11111111-2222-3333-4444-555555555555", "client_id": "abc", "is_active": true, "token": PAT, "token_label": "desktop-generated", "scopes": ["repo:admin"]}}),
    )
    .await;
    let stand_in = given["data"]["token"].as_str().unwrap();
    assert_ne!(stand_in, PAT);
    assert!(stand_in.starts_with("dckr_pat_"));
    assert_eq!(stand_in.len(), PAT.len());
    assert_eq!(
        given["data"]["uuid"],
        "11111111-2222-3333-4444-555555555555"
    );
    assert_eq!(given["data"]["scopes"], json!(["repo:admin"]));
    assert_eq!(kept(&rig, Role::ApiKey).unwrap().1, PAT);
    assert_eq!(kept(&rig, Role::ApiKey).unwrap().0, stand_in);
    assert_eq!(rig.registry.len(), 4);
}

#[tokio::test]
async fn a_pat_answer_with_no_token_in_it_keeps_nothing() {
    use puddle_proxy::AnswerHead;
    let rig = rig_for(&PROFILES);
    let mut exchange = rig
        .logins
        .begin(
            &host("hub.docker.com"),
            "POST",
            "/v2/access-tokens/desktop-generate",
        )
        .unwrap();
    let bytes = br#"{"data":{"uuid":"abc","is_active":false}}"#;
    let head = AnswerHead {
        status: 201,
        content_type: Some("application/json"),
        content_encoding: None,
        content_length: Some(bytes.len() as u64),
    };
    let answer = exchange.answer_mut().unwrap();
    assert!(answer.rewrite(&head, bytes).await.is_none());
    assert!(kept(&rig, Role::ApiKey).is_none());
    assert_eq!(rig.registry.len(), 0);
}

#[tokio::test]
async fn forgetting_the_login_deletes_all_four_tokens() {
    let rig = rig_for(&PROFILES);
    poll(&rig, &access_token()).await;
    answered(
        &rig,
        "hub.docker.com",
        "POST",
        "/v2/access-tokens/desktop-generate",
        &json!({"data": {"token": PAT}}),
    )
    .await;
    assert!(rig.logins.forget("docker").await.unwrap());
    for role in [Role::Access, Role::Refresh, Role::ApiKey, Role::IdToken] {
        assert!(kept(&rig, role).is_none(), "{role:?}");
    }
    assert_eq!(rig.registry.len(), 0);
}

/// A bearer token as `auth.docker.io` mints one: a JWT for one repository and five minutes. The
/// claims are the ones the live service puts in it; `n` makes each one different.
fn registry_token(n: u64) -> String {
    jwt(&json!({
        "iss": "auth.docker.io", "aud": "registry.docker.io", "sub": "someone",
        "iat": 1_893_456_000 + n, "nbf": 1_893_456_000 + n, "exp": 1_893_456_300 + n,
        "jti": format!("jti-{n}"),
        "access": [{"type": "repository", "name": "someone/app", "actions": ["pull", "push"]}],
    }))
}

/// The token server's answer, in the shape the live service gives: the token twice.
async fn registry_answer(rig: &Rig, method: &str, real: &str) -> Value {
    answered(
        rig,
        "auth.docker.io",
        method,
        "/token",
        &json!({"token": real, "access_token": real, "expires_in": 300, "issued_at": "2030-01-01T00:00:00.000000000Z"}),
    )
    .await
}

fn swapped(rig: &Rig, stand_in: &str, real: &str) -> bool {
    rig.registry
        .set_real(stand_in, puddle_proxy::SecretValue::new(real))
        == Ok(true)
}

#[tokio::test]
async fn a_registry_token_is_replaced_held_in_memory_and_swapped_toward_the_registry_only() {
    let rig = rig_for(&PROFILES);
    // A locked credential store does not stop a pull: nothing is written to it.
    rig.store.break_it();
    let real = registry_token(0);
    let given = registry_answer(&rig, "GET", &real).await;
    let stand_in = given["token"].as_str().unwrap();
    assert_eq!(
        given["access_token"], given["token"],
        "one stand-in for both"
    );
    assert_ne!(stand_in, real);
    assert!(!given.to_string().contains(real.rsplit('.').next().unwrap()));
    // The client reads the claims (the expiry); they are kept.
    assert_eq!(part(stand_in, 1), part(&real, 1));
    assert_eq!(given["expires_in"], 300);
    assert!(swapped(&rig, stand_in, &real));
    assert_eq!(rig.registry.len(), 1);
    let hosts = rig.registry.hosts();
    assert!(hosts.contains(&host("registry-1.docker.io")));
    for other in [
        "auth.docker.io",
        "hub.docker.com",
        "login.docker.com",
        "docker.io",
    ] {
        assert!(!hosts.contains(&host(other)), "{other}");
    }
    // Not a login: nothing is listed as kept and nothing is in the store.
    assert_eq!(rig.logins.kept(), []);
    rig.store.heal();
    assert_eq!(rig.store.ids(), [] as [String; 0]);
    assert!(
        kept(
            &rig,
            Slot {
                role: Role::Access,
                resource: Some("registry")
            }
        )
        .is_none()
    );
    // The `OAuth2` form of the same request is read the same way.
    let real = registry_token(1);
    let given = registry_answer(&rig, "POST", &real).await;
    assert!(swapped(&rig, given["token"].as_str().unwrap(), &real));
}

#[tokio::test]
async fn many_pulls_at_once_keep_their_tokens_swapped_up_to_a_bound() {
    let rig = rig_for(&PROFILES);
    let total = MEMORY_KEPT + 3;
    let mut given = Vec::new();
    for n in 0..total {
        let real = registry_token(n as u64);
        let answer = registry_answer(&rig, "GET", &real).await;
        given.push((answer["token"].as_str().unwrap().to_owned(), real));
    }
    let distinct: std::collections::BTreeSet<&String> = given.iter().map(|(s, _)| s).collect();
    assert_eq!(distinct.len(), total, "every token has its own stand-in");
    // The newest and the MEMORY_KEPT before it are swapped; the oldest ones are not.
    for (n, (stand_in, real)) in given.iter().enumerate() {
        assert_eq!(
            swapped(&rig, stand_in, real),
            n >= total - MEMORY_KEPT - 1,
            "{n}"
        );
    }
    assert_eq!(rig.registry.len(), MEMORY_KEPT + 1);
}

#[tokio::test]
async fn forgetting_the_login_also_stops_swapping_the_registry_tokens() {
    let rig = rig_for(&PROFILES);
    poll(&rig, &access_token()).await;
    registry_answer(&rig, "GET", &registry_token(0)).await;
    registry_answer(&rig, "GET", &registry_token(1)).await;
    assert_eq!(rig.registry.len(), 5);
    assert!(rig.logins.forget("docker").await.unwrap());
    assert_eq!(rig.registry.len(), 0);
}

#[tokio::test]
async fn an_access_token_created_through_docker_hubs_api_is_kept_apart_from_the_logins_own() {
    const CREATED: &str = "dckr_pat_CANARY0created0one0AAAAA";
    const CREATED_AGAIN: &str = "dckr_pat_CANARY0created0two0AAAAA";
    const ORG: &str = "dckr_oat_CANARY0org0token0AAAAAAA";
    let rig = rig_for(&PROFILES);
    poll(&rig, &access_token()).await;
    let login_pat = answered(
        &rig,
        "hub.docker.com",
        "POST",
        "/v2/access-tokens/desktop-generate",
        &json!({"data": {"token": PAT}}),
    )
    .await;
    // The answer's shape is the one Docker Hub's API description gives for a new token.
    let create = |token: &str| {
        json!({"uuid": "b30bbf97-506c-4ecd-aabc-842f3cb484fb", "client_id": "HUB", "creator_ip": "127.0.0.1",
            "created_at": "2030-01-01T00:00:00Z", "last_used": null, "generated_by": "manual",
            "is_active": true, "token": token, "token_label": "ci", "scopes": ["repo:read"], "expires_at": null})
    };
    let first = answered(
        &rig,
        "hub.docker.com",
        "POST",
        "/v2/access-tokens",
        &create(CREATED),
    )
    .await;
    let stand_in = first["token"].as_str().unwrap();
    assert_ne!(stand_in, CREATED);
    assert!(stand_in.starts_with("dckr_pat_"));
    assert_eq!(first["token_label"], "ci");
    let created = Slot {
        role: Role::ApiKey,
        resource: Some("created"),
    };
    assert_eq!(
        kept(&rig, created).unwrap(),
        (stand_in.to_owned(), CREATED.to_owned())
    );
    // The login's own PAT is untouched.
    assert_eq!(
        kept(&rig, Role::ApiKey).unwrap(),
        (
            login_pat["data"]["token"].as_str().unwrap().to_owned(),
            PAT.to_owned()
        )
    );
    // A second token of the kind: the same stand-in, the newer token behind it.
    let second = answered(
        &rig,
        "hub.docker.com",
        "POST",
        "/v2/access-tokens",
        &create(CREATED_AGAIN),
    )
    .await;
    assert_eq!(second["token"], first["token"]);
    assert!(swapped(&rig, stand_in, CREATED_AGAIN));
    assert_eq!(kept(&rig, created).unwrap().1, CREATED_AGAIN);
    // An organization's token.
    let org = answered(
        &rig,
        "hub.docker.com",
        "POST",
        "/v2/orgs/acme/access-tokens",
        &json!({"id": "a7a5ef25-8889-43a0-8cc7-f2a94268e861", "label": "ci", "is_active": true,
            "token": ORG, "resources": [{"type": "TYPE_REPO", "path": "acme/app", "scopes": ["scope-image-pull"]}]}),
    )
    .await;
    let org_stand_in = org["token"].as_str().unwrap();
    assert!(org_stand_in.starts_with("dckr_oat_"));
    assert_ne!(org_stand_in, ORG);
    assert_eq!(
        kept(
            &rig,
            Slot {
                role: Role::ApiKey,
                resource: Some("org")
            }
        )
        .unwrap()
        .1,
        ORG
    );
    assert!(!org.to_string().contains(ORG));
}
