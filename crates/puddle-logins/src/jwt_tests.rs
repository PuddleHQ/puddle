// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests of a field whose stand-in is a JWT ([`StandInKind::Jwt`]): a refresh makes a new stand-in, and
//! the slot keeps the last few older ones registered.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use puddle_types::WorkspaceId;
use serde_json::{Value, json};

use crate::profile::{Audience, Endpoint, Field, Keep, Method, Profile, Role, Scope, StandInKind};
use crate::vault::Vault;
use crate::widened_tests::{Rig, answered, rig_for};

static JWT_PROFILE: [Profile; 1] = [Profile {
    id: "jwt",
    name: "Jwt",
    endpoints: &[Endpoint {
        host: "login.jwt.test",
        method: Method::Post,
        path: "/token",
        request_fields: &["refresh_token"],
        fields: &[
            Field {
                path: "access_token",
                role: Role::Access,
                resource: None,
                prefixes: &[],
                keep_last: 0,
                stand_in: StandInKind::Jwt,
                only_prefixed: false,
                scope: Scope::Api,
                audience: Audience::Any,
                keep: Keep::Stored,
            },
            Field {
                path: "refresh_token",
                role: Role::Refresh,
                resource: None,
                prefixes: &["rt_"],
                keep_last: 0,
                stand_in: StandInKind::Shaped,
                only_prefixed: false,
                scope: Scope::TokenEndpoint,
                audience: Audience::Any,
                keep: Keep::Stored,
            },
        ],
    }],
    api_hosts: &["api.jwt.test"],
}];

fn jwt(exp: u64, signature: &str) -> String {
    format!(
        "{}.{}.{signature}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(json!({"sub": "me", "exp": exp}).to_string()),
    )
}

async fn token(rig: &Rig, access: &str, refresh: &str) -> Value {
    answered(
        rig,
        "login.jwt.test",
        "POST",
        "/token",
        &json!({"access_token": access, "refresh_token": refresh, "expires_in": 3600}),
    )
    .await
}

fn claims_of(token: &str) -> Value {
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
}

const REFRESH: &str = "rt_CANARY0refresh0AAAAAAAAAAAAAAAAAAAAAAAA";

#[tokio::test]
async fn a_jwt_login_gives_the_tool_a_token_whose_claims_read_the_same() {
    let rig = rig_for(&JWT_PROFILE);
    let real = jwt(1_893_456_000, &"S".repeat(86));
    let given = token(&rig, &real, REFRESH).await;
    let access = given["access_token"].as_str().unwrap();
    assert_ne!(access, real);
    assert_eq!(access.len(), real.len());
    assert_eq!(
        claims_of(access),
        json!({"sub": "me", "exp": 1_893_456_000})
    );
    assert_eq!(
        access.rsplit_once('.').unwrap().0,
        real.rsplit_once('.').unwrap().0
    );
    assert_eq!(given["expires_in"], 3600);
    assert_eq!(rig.registry.len(), 2);
    let kept = Vault::new(
        std::sync::Arc::clone(&rig.store) as _,
        &WorkspaceId::new("alpha").unwrap(),
    )
    .read(&JWT_PROFILE[0], Role::Access)
    .unwrap()
    .unwrap();
    assert_eq!(
        (kept.stand_in.as_str(), kept.real.as_str()),
        (access, real.as_str())
    );
}

#[tokio::test]
async fn a_refresh_makes_a_new_stand_in_and_a_few_older_ones_stay_registered() {
    let rig = rig_for(&JWT_PROFILE);
    let mut given = Vec::new();
    let mut last_real = String::new();
    for (n, letter) in (0_u64..).zip(["A", "B", "C", "D", "E"]) {
        last_real = jwt(1_000_000_001 + 3600 * n, &letter.repeat(86));
        given.push(
            token(&rig, &last_real, REFRESH).await["access_token"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    for (a, stand_in) in given.iter().enumerate() {
        assert_eq!(
            claims_of(stand_in)["exp"],
            1_000_000_001 + 3600 * u64::try_from(a).unwrap()
        );
        assert!(given[..a].iter().all(|earlier| earlier != stand_in));
    }
    // The newest and the three before it, and the one refresh stand-in, which a refresh keeps.
    assert_eq!(rig.registry.len(), 5);
    let registered = |stand_in: &str| {
        rig.registry
            .set_real(stand_in, puddle_proxy::SecretValue::new(last_real.as_str()))
            == Ok(true)
    };
    assert!(!registered(&given[0]), "the oldest is no longer swapped");
    for stand_in in &given[1..] {
        assert!(registered(stand_in), "{stand_in}");
    }
    // The store holds the newest pair only.
    let kept = Vault::new(
        std::sync::Arc::clone(&rig.store) as _,
        &WorkspaceId::new("alpha").unwrap(),
    )
    .read(&JWT_PROFILE[0], Role::Access)
    .unwrap()
    .unwrap();
    assert_eq!(
        (kept.stand_in.as_str(), kept.real.as_str()),
        (given[4].as_str(), last_real.as_str())
    );
}

#[tokio::test]
async fn a_long_run_of_refreshes_never_holds_more_than_the_bound() {
    let rig = rig_for(&JWT_PROFILE);
    for n in 0_u64..60 {
        token(
            &rig,
            &jwt(1_000_000_001 + 3600 * n, &format!("{n:A>86}")),
            REFRESH,
        )
        .await;
        // The newest, the kept older ones and the one refresh-token stand-in, however long it runs.
        assert!(
            rig.registry.len() <= 1 + crate::logins::RETIRED_KEPT + 1,
            "refresh {n}"
        );
    }
    assert_eq!(rig.registry.len(), 1 + crate::logins::RETIRED_KEPT + 1);
}

#[tokio::test]
async fn a_token_that_is_not_a_jwt_keeps_its_stand_in_through_a_refresh() {
    let rig = rig_for(&JWT_PROFILE);
    let first = token(&rig, "EwB-opaque-token-0123456789abcdefghij", REFRESH).await;
    let second = token(&rig, "EwB-opaque-token-second-abcdefghijkl", REFRESH).await;
    assert_eq!(first["access_token"], second["access_token"]);
    assert_eq!(rig.registry.len(), 2);
}

#[tokio::test]
async fn forgetting_removes_every_stand_in_and_a_restart_registers_the_newest_only() {
    let rig = rig_for(&JWT_PROFILE);
    token(&rig, &jwt(1_000_000_001, &"A".repeat(86)), REFRESH).await;
    token(&rig, &jwt(1_000_003_601, &"B".repeat(86)), REFRESH).await;
    assert_eq!(rig.registry.len(), 3);

    let restarted = rig_for_store(&rig);
    restarted.logins.load().await;
    assert_eq!(restarted.registry.len(), 2);

    assert!(rig.logins.forget("jwt").await.unwrap());
    assert_eq!(rig.registry.len(), 0);
}

fn rig_for_store(old: &Rig) -> Rig {
    let registry = std::sync::Arc::new(puddle_proxy::StandIns::new());
    let logins = crate::Logins::new(
        WorkspaceId::new("alpha").unwrap(),
        true,
        &JWT_PROFILE,
        std::sync::Arc::clone(&registry),
        std::sync::Arc::clone(&old.store) as _,
        std::sync::Arc::new(crate::widened_tests::Quiet),
        crate::Marker::none(),
    );
    Rig {
        logins,
        store: std::sync::Arc::clone(&old.store),
        registry,
    }
}
