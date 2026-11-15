// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `OpenAI` Codex profile: the login and the refresh answer with two JWTs and a refresh token, and
//! the token exchange that trades the identity token for an API key answers in the same
//! `access_token` field as the login does.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use puddle_proxy::ExchangeRewriter as _;
use puddle_types::WorkspaceId;
use serde_json::{Value, json};

use crate::profile::{OPENAI, Role, Scope, Slot};
use crate::vault::Vault;
use crate::widened_tests::{Rig, answered, host, rig_for};

static PROFILES: [crate::profile::Profile; 1] = [OPENAI];

const REFRESH: &str = "rt_CANARY0openai0refresh.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const API_KEY: &str =
    "sk-proj-CANARY0openai0api0key0AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn jwt(claims: &Value, signature_len: usize) -> String {
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
        "S".repeat(signature_len),
    )
}

fn claims_of(token: &str) -> Value {
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
}

fn id_token() -> String {
    jwt(
        &json!({"email": "me@example.test", "exp": 1_893_456_000, "https://api.openai.com/auth": {"chatgpt_plan_type": "plus"}}),
        342,
    )
}

fn access_token() -> String {
    jwt(&json!({"exp": 1_893_456_000, "pad": "p".repeat(1500)}), 342)
}

async fn exchange(rig: &Rig, body: &Value) -> Value {
    answered(rig, "auth.openai.com", "POST", "/oauth/token", body).await
}

fn kept(rig: &Rig, role: Role) -> Option<(String, String)> {
    Vault::new(
        Arc::clone(&rig.store) as _,
        &WorkspaceId::new("alpha").unwrap(),
    )
    .read(&OPENAI, role)
    .unwrap()
    .map(|saved| (saved.stand_in.clone(), saved.real.to_string()))
}

#[test]
fn the_openai_profile_has_four_slots_with_their_scopes_and_entry_names() {
    let slots: Vec<(&str, Scope)> = OPENAI
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
    assert_eq!(
        OPENAI.slot_name_of(Slot::of(Role::IdToken)),
        "openai-id-token"
    );
    let id = crate::vault::slot_id("alpha", "openai", Role::IdToken).unwrap();
    assert!(id.as_str().ends_with("-openai-id-token"), "{}", id.as_str());
}

#[test]
fn the_identity_and_refresh_tokens_are_swapped_toward_the_token_host_only() {
    let hosts = OPENAI.hosts_for(Scope::TokenEndpoint);
    assert!(hosts.contains(&host("auth.openai.com")));
    assert!(!hosts.contains(&host("api.openai.com")));
    assert!(!hosts.contains(&host("chatgpt.com")));
    let api = OPENAI.hosts_for(Scope::Api);
    for name in ["auth.openai.com", "api.openai.com", "chatgpt.com"] {
        assert!(api.contains(&host(name)), "{name}");
    }
    assert!(!api.contains(&host("platform.openai.com")));
    assert!(!api.contains(&host("evilchatgpt.com")));
}

#[test]
fn the_refresh_token_and_the_identity_token_are_swapped_in_the_request() {
    let rig = rig_for(&PROFILES);
    let exchange = rig
        .logins
        .begin(&host("auth.openai.com"), "POST", "/oauth/token")
        .unwrap();
    assert_eq!(exchange.swap_fields(), ["refresh_token", "subject_token"]);
    assert!(
        rig.logins
            .begin(&host("auth.openai.com"), "GET", "/oauth/token")
            .is_none()
    );
    assert!(
        rig.logins
            .begin(&host("auth.openai.com"), "POST", "/oauth/authorize")
            .is_none()
    );
}

#[tokio::test]
async fn a_login_gives_codex_jwts_with_the_real_claims_and_a_shaped_refresh_token() {
    let rig = rig_for(&PROFILES);
    let (id, access) = (id_token(), access_token());
    let given = exchange(
        &rig,
        &json!({"id_token": id, "access_token": access, "refresh_token": REFRESH}),
    )
    .await;
    let (given_id, given_access, given_refresh) = (
        given["id_token"].as_str().unwrap(),
        given["access_token"].as_str().unwrap(),
        given["refresh_token"].as_str().unwrap(),
    );
    for (given, real) in [(given_id, &id), (given_access, &access)] {
        assert_ne!(given, real);
        assert_eq!(given.len(), real.len());
        assert_eq!(claims_of(given), claims_of(real));
    }
    assert!(access.len() > 1024, "a ChatGPT access token is long");
    assert_ne!(given_refresh, REFRESH);
    assert!(given_refresh.starts_with("rt_"));
    assert_eq!(given_refresh.len(), REFRESH.len());
    for role in [Role::Access, Role::IdToken, Role::Refresh] {
        assert!(kept(&rig, role).is_some(), "{role:?}");
    }
    assert!(kept(&rig, Role::ApiKey).is_none());
    assert_eq!(kept(&rig, Role::IdToken).unwrap().1, id);
    assert_eq!(rig.registry.len(), 3);
    let text = given.to_string();
    for real in [&id, &access] {
        assert!(!text.contains(real.rsplit('.').next().unwrap()));
    }
    assert!(!text.contains(REFRESH));
}

#[tokio::test]
async fn the_token_exchange_answer_is_an_api_key_and_leaves_the_access_token_alone() {
    let rig = rig_for(&PROFILES);
    let access = access_token();
    let login = exchange(
        &rig,
        &json!({"id_token": id_token(), "access_token": access, "refresh_token": REFRESH}),
    )
    .await;
    let given = exchange(
        &rig,
        &json!({"access_token": API_KEY, "issued_token_type": "urn:ietf:params:oauth:token-type:access_token", "token_type": "Bearer", "expires_in": 3600}),
    )
    .await;
    let key = given["access_token"].as_str().unwrap();
    assert_ne!(key, API_KEY);
    assert!(key.starts_with("sk-"));
    assert_eq!(key.len(), API_KEY.len());
    assert_eq!(given["token_type"], "Bearer");
    assert_eq!(kept(&rig, Role::ApiKey).unwrap().1, API_KEY);
    // The login's access token is still the one it was.
    assert_eq!(kept(&rig, Role::Access).unwrap().1, access);
    assert_eq!(kept(&rig, Role::Access).unwrap().0, login["access_token"]);
    assert_eq!(rig.registry.len(), 4);
}

#[tokio::test]
async fn a_refresh_gives_new_claims_new_stand_ins_and_keeps_the_older_ones_swapped() {
    let rig = rig_for(&PROFILES);
    let first = exchange(
        &rig,
        &json!({"id_token": id_token(), "access_token": access_token(), "refresh_token": REFRESH}),
    )
    .await;
    let new_access = jwt(
        &json!({"exp": 1_893_459_600_u64, "pad": "q".repeat(1500)}),
        342,
    );
    let second = exchange(
        &rig,
        &json!({"id_token": id_token(), "access_token": new_access, "refresh_token": REFRESH}),
    )
    .await;
    assert_ne!(first["access_token"], second["access_token"]);
    assert_eq!(
        claims_of(second["access_token"].as_str().unwrap())["exp"],
        1_893_459_600_u64
    );
    // The refresh token keeps its stand-in; each JWT has one more.
    assert_eq!(first["refresh_token"], second["refresh_token"]);
    assert_eq!(rig.registry.len(), 5);
}

#[tokio::test]
async fn an_access_token_that_is_neither_a_jwt_nor_a_key_is_shaped_and_never_passes() {
    let rig = rig_for(&PROFILES);
    let opaque = "ACCESSCANARY0opaque0AAAAAAAAAAAAAAAAAAAAAAAA";
    let given = exchange(&rig, &json!({"access_token": opaque})).await;
    let stand_in = given["access_token"].as_str().unwrap();
    assert_ne!(stand_in, opaque);
    assert_eq!(stand_in.len(), opaque.len());
    assert_eq!(kept(&rig, Role::Access).unwrap().1, opaque);
    assert!(kept(&rig, Role::ApiKey).is_none());
}

#[tokio::test]
async fn forgetting_the_login_deletes_all_four_tokens() {
    let rig = rig_for(&PROFILES);
    exchange(
        &rig,
        &json!({"id_token": id_token(), "access_token": access_token(), "refresh_token": REFRESH}),
    )
    .await;
    exchange(&rig, &json!({"access_token": API_KEY})).await;
    assert_eq!(
        rig.logins.kept()[0].roles,
        [Role::Access, Role::Refresh, Role::ApiKey, Role::IdToken]
    );
    assert!(rig.logins.forget("openai").await.unwrap());
    for role in [Role::Access, Role::Refresh, Role::ApiKey, Role::IdToken] {
        assert!(kept(&rig, role).is_none(), "{role:?}");
    }
    assert_eq!(rig.registry.len(), 0);
}
