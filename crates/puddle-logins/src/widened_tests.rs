// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests of the widened profile model: a method other than `POST`, a wildcard path segment, a
//! nested answer field and a slot per resource, on a profile made for the test.

use std::sync::Arc;

use puddle_proxy::{AnswerHead, ExchangeRewriter, StandIns};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::{Host, WorkspaceId};
use serde_json::{Value, json};

use crate::profile::{Endpoint, Field, Method, Profile, Role, Scope, Slot, path_matches};
use crate::vault::{Vault, slot_id};
use crate::{LoginNotices, Logins, Marker, Problem};

const ARM_REAL: &str = "eyARMCANARY0real0AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const GRAPH_REAL: &str = "eyGRAPHCANARY0real0BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const DEEP_REAL: &str = "npm_CANARY0deep0real0CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

const fn field(path: &'static str, role: Role, resource: Option<&'static str>) -> Field {
    Field {
        path,
        role,
        resource,
        prefixes: &["ey", "npm_"],
        keep_last: 0,
        scope: Scope::Api,
    }
}

static WIDE: [Profile; 1] = [Profile {
    id: "wide",
    name: "Wide",
    endpoints: &[
        Endpoint {
            host: "login.wide.test",
            method: Method::Post,
            path: "/*/oauth2/token",
            refresh_field: Some("refresh_token"),
            fields: &[
                field("arm_token", Role::Access, Some("arm")),
                field("graph_token", Role::Access, Some("graph")),
            ],
        },
        Endpoint {
            host: "registry.wide.test",
            method: Method::Any,
            path: "/-/user/org.couchdb.user:*",
            refresh_field: None,
            fields: &[field("data.deep.token", Role::ApiKey, None)],
        },
    ],
    api_hosts: &["api.wide.test"],
}];

struct Quiet;
impl LoginNotices for Quiet {
    fn problem(&self, _: &WorkspaceId, _: &Profile, _: Problem) {}
}

struct Rig {
    logins: Logins,
    store: Arc<MemoryStore>,
}

fn rig() -> Rig {
    let store = Arc::new(MemoryStore::new());
    let logins = Logins::new(
        WorkspaceId::new("alpha").unwrap(),
        true,
        &WIDE,
        Arc::new(StandIns::new()),
        Arc::clone(&store) as Arc<dyn SecretStore>,
        Arc::new(Quiet),
        Marker::none(),
    );
    Rig { logins, store }
}

fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap()
}

async fn answered(rig: &Rig, host_name: &str, method: &str, path: &str, body: &Value) -> Value {
    let mut exchange = rig
        .logins
        .begin(&host(host_name), method, path)
        .expect("the request is an exchange");
    let bytes = serde_json::to_vec(body).unwrap();
    let head = AnswerHead {
        status: 200,
        content_type: Some("application/json"),
        content_encoding: None,
        content_length: Some(bytes.len() as u64),
    };
    let answer = exchange.answer_mut().unwrap();
    assert!(answer.wants(&head));
    let out = answer.rewrite(&head, &bytes).await.expect("captured");
    serde_json::from_slice(&out).unwrap()
}

#[test]
fn a_wildcard_segment_matches_one_non_empty_segment_and_nothing_else() {
    assert!(path_matches("/*/oauth2/token", "/tenant-1/oauth2/token"));
    assert!(!path_matches("/*/oauth2/token", "/oauth2/token"));
    assert!(!path_matches("/*/oauth2/token", "/a/b/oauth2/token"));
    assert!(!path_matches("/*/oauth2/token", "//oauth2/token"));
    assert!(path_matches(
        "/-/user/org.couchdb.user:*",
        "/-/user/org.couchdb.user:me"
    ));
    assert!(!path_matches(
        "/-/user/org.couchdb.user:*",
        "/-/user/org.couchdb.user:"
    ));
    assert!(!path_matches(
        "/-/user/org.couchdb.user:*",
        "/-/user/other:me"
    ));
    assert!(path_matches("/v1/token", "/v1/token"));
    assert!(!path_matches("/v1/token", "/v1/token/x"));
}

#[test]
fn a_method_covers_what_it_says() {
    assert!(Method::Post.accepts("POST") && !Method::Post.accepts("GET"));
    assert!(Method::Get.accepts("GET") && !Method::Get.accepts("POST"));
    assert!(Method::Put.accepts("PUT") && !Method::Put.accepts("POST"));
    assert!(Method::Any.accepts("PATCH"));
}

#[tokio::test]
async fn an_endpoint_is_an_exchange_only_for_its_method_and_a_wildcard_path() {
    let rig = rig();
    let token = |m, p| rig.logins.begin(&host("login.wide.test"), m, p).is_some();
    assert!(token("POST", "/contoso/oauth2/token"));
    assert!(token("POST", "/Contoso/OAuth2/Token"));
    assert!(!token("GET", "/contoso/oauth2/token"));
    assert!(!token("POST", "/oauth2/token"));
    let any = |m| {
        rig.logins
            .begin(
                &host("registry.wide.test"),
                m,
                "/-/user/org.couchdb.user:me",
            )
            .is_some()
    };
    assert!(any("GET") && any("PUT") && any("POST"));
}

#[tokio::test]
async fn a_nested_answer_field_is_replaced_in_place() {
    let rig = rig();
    let body = json!({"data": {"deep": {"token": DEEP_REAL, "keep": 1}}, "ok": true});
    let out = answered(
        &rig,
        "registry.wide.test",
        "PUT",
        "/-/user/org.couchdb.user:me",
        &body,
    )
    .await;
    let stand_in = out["data"]["deep"]["token"].as_str().unwrap();
    assert_ne!(stand_in, DEEP_REAL);
    assert!(stand_in.starts_with("npm_"));
    assert_eq!(stand_in.len(), DEEP_REAL.len());
    assert_eq!(out["data"]["deep"]["keep"], 1);
    assert_eq!(out["ok"], true);
}

#[tokio::test]
async fn two_resources_of_one_role_get_a_stand_in_and_an_entry_each() {
    let rig = rig();
    let out = answered(
        &rig,
        "login.wide.test",
        "POST",
        "/contoso/oauth2/token",
        &json!({"arm_token": ARM_REAL, "graph_token": GRAPH_REAL}),
    )
    .await;
    let (arm, graph) = (
        out["arm_token"].as_str().unwrap(),
        out["graph_token"].as_str().unwrap(),
    );
    assert_ne!(arm, graph);
    let vault = Vault::new(
        Arc::clone(&rig.store) as Arc<dyn SecretStore>,
        &WorkspaceId::new("alpha").unwrap(),
    );
    let arm_slot = Slot {
        role: Role::Access,
        resource: Some("arm"),
    };
    let graph_slot = Slot {
        role: Role::Access,
        resource: Some("graph"),
    };
    let saved = vault.read(&WIDE[0], arm_slot).unwrap().unwrap();
    assert_eq!(saved.stand_in, arm);
    assert_eq!(&*saved.real, ARM_REAL);
    assert_eq!(
        &*vault.read(&WIDE[0], graph_slot).unwrap().unwrap().real,
        GRAPH_REAL
    );
    assert!(vault.read(&WIDE[0], Role::Access).unwrap().is_none());
    // A second answer keeps each resource's stand-in.
    let again = answered(
        &rig,
        "login.wide.test",
        "POST",
        "/contoso/oauth2/token",
        &json!({"arm_token": GRAPH_REAL, "graph_token": ARM_REAL}),
    )
    .await;
    assert_eq!(again["arm_token"], out["arm_token"]);
    assert_eq!(again["graph_token"], out["graph_token"]);
}

#[tokio::test]
async fn forgetting_deletes_every_resources_entry() {
    let rig = rig();
    answered(
        &rig,
        "login.wide.test",
        "POST",
        "/contoso/oauth2/token",
        &json!({"arm_token": ARM_REAL, "graph_token": GRAPH_REAL}),
    )
    .await;
    assert_eq!(rig.logins.kept().len(), 1);
    assert!(rig.logins.forget("wide").await.unwrap());
    assert_eq!(rig.logins.kept().len(), 0);
    for resource in ["arm", "graph"] {
        let id = slot_id(
            "alpha",
            "wide",
            Slot {
                role: Role::Access,
                resource: Some(resource),
            },
        )
        .unwrap();
        assert!(rig.store.get(&id).unwrap().is_none(), "{resource}");
    }
}

#[test]
fn slots_are_listed_once_each_and_a_role_with_two_resources_counts_once() {
    let slots: Vec<Slot> = WIDE[0].slots().into_iter().map(|(slot, _)| slot).collect();
    assert_eq!(slots.len(), 3);
    assert_eq!(WIDE[0].roles(), [Role::Access, Role::ApiKey]);
    assert_eq!(
        WIDE[0].slot_name_of(Slot {
            role: Role::Access,
            resource: Some("arm")
        }),
        "wide-access-arm"
    );
    assert_eq!(WIDE[0].slot_name_of(Slot::of(Role::ApiKey)), "wide-api-key");
}

#[test]
fn every_built_in_slot_resource_fits_an_entry_name() {
    for profile in crate::builtin().iter().chain(WIDE.iter()) {
        for (slot, _) in profile.slots() {
            if let Some(resource) = slot.resource {
                assert!(resource.len() <= 12, "{}: {resource}", profile.id);
                assert!(
                    resource
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                    "{}: {resource}",
                    profile.id
                );
            }
            assert!(slot_id("a-very-long-workspace-name-indeed", profile.id, slot).is_some());
        }
    }
}
