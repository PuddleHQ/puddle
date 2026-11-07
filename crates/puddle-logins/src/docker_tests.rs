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

use crate::profile::{DOCKER, Role, Scope};
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

fn kept(rig: &Rig, role: Role) -> Option<(String, String)> {
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
fn the_docker_profile_has_four_slots_with_their_scopes() {
    let slots: Vec<(&str, Scope)> = DOCKER
        .slots()
        .into_iter()
        .map(|(slot, scope)| (slot.role.key(), scope))
        .collect();
    assert_eq!(
        slots,
        [
            ("access", Scope::Api),
            ("refresh", Scope::TokenEndpoint),
            ("api-key", Scope::Api),
            ("id-token", Scope::TokenEndpoint),
        ]
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
    for (method, path) in [
        ("GET", "/v2/access-tokens/desktop-generate"),
        ("POST", "/v2/access-tokens"),
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
