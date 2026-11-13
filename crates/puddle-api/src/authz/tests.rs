// SPDX-License-Identifier: GPL-3.0-or-later
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::Duration;

use puddle_store::{Limits, ManualClock, UserName};
use puddle_types::{EgressRequest, Host, SuffixAllows};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;
use crate::events::EventHub;
use crate::learned::LearnedService as _;
use crate::server::{ApiConfig, ApiServer, RunningApi, Services};
use crate::settings::{MemorySettings, SettingsRepo};
use crate::token::ApiToken;
use crate::{FakeCredentials, FakeLearned, FakeWorkspaces, openapi};
use puddle_secrets::MemoryStore;

const TOKEN_STEP: Duration = Duration::from_secs(10);

fn ann_principal(user: UserId) -> Principal {
    Principal {
        user,
        role: Role::User,
        via: Via::Device(DeviceId::from_stored(1)),
    }
}

/// Every operation the contract lists, plus the contract itself, as `(method, path)`.
fn operations() -> Vec<(Method, String)> {
    let spec = serde_json::to_value(openapi()).unwrap();
    let mut out = vec![(Method::GET, "/api/openapi.json".to_owned())];
    for (path, item) in spec["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            out.push((method.to_uppercase().parse().unwrap(), path.clone()));
        }
    }
    out
}

#[test]
fn every_route_declares_an_action_and_a_resource() {
    let operations = operations();
    assert_eq!(
        operations.len(),
        openapi::committed_operation_count() + 1,
        "every committed operation and the contract itself"
    );
    let missing: Vec<String> = operations
        .iter()
        .filter(|(method, path)| declaration(method, path).is_none())
        .map(|(method, path)| format!("{method} {path}"))
        .collect();
    assert!(
        missing.is_empty(),
        "routes with no declaration: {missing:?}"
    );
    // A path parameter the declaration names is one the route has.
    for (method, path) in &operations {
        if let Some(Declared {
            target: Target::Workspace(name),
            ..
        }) = declaration(method, path)
        {
            assert!(path.contains(&format!("{{{name}}}")), "{method} {path}");
        }
    }
    assert_eq!(declaration(&Method::GET, "/api/nothing-here"), None);
}

/// The resource a path names by its first parameter, from the path alone.
fn named_by(path: &str) -> Option<Target> {
    Some(match path {
        "/api/workspaces/{id}/terminals/{terminal}" => Target::Ticket,
        p if p.starts_with("/api/workspaces/{id}") => Target::Workspace("id"),
        p if p.starts_with("/api/settings/workspaces/{workspace}") => {
            Target::Workspace("workspace")
        }
        p if p.starts_with("/api/pending/{id}") => Target::Pending,
        p if p.starts_with("/api/rules/{id}") => Target::Rule,
        p if p.starts_with("/api/request-rules/{id}") => Target::RequestRule,
        _ => return None,
    })
}

/// The converse of [`every_route_declares_an_action_and_a_resource`]: a route whose path names a
/// workspace, a request or a rule declares exactly that resource, so the check looks it up for the
/// principal (a route under a workspace's path declared as the principal's own collection would
/// run with no owner check), and only such a route declares one. Every other route with a
/// parameter is listed here by hand: its id is looked up in the principal's own collection by
/// the handler, or the route is the owner's alone, so a new one has to be placed on purpose.
#[test]
fn every_route_that_names_a_resource_declares_it_and_no_other_does() {
    let mut by_hand: Vec<String> = Vec::new();
    for (method, path) in operations() {
        let declared = declaration(&method, &path).unwrap();
        if let Some(target) = named_by(&path) {
            assert_eq!(declared.target, target, "{method} {path}");
            continue;
        }
        assert!(
            matches!(
                declared.target,
                Target::Own | Target::Install | Target::Service
            ),
            "{method} {path} declares {:?}, which its path does not name",
            declared.target
        );
        if path.contains('{') {
            by_hand.push(format!("{method} {path} {:?}", declared.target));
        }
    }
    by_hand.sort();
    assert_eq!(
        by_hand,
        [
            "DELETE /api/credentials/stored/{id} Own",
            "DELETE /api/devices/{id} Own",
            "DELETE /api/env/{name} Own",
            "DELETE /api/identities/{id} Own",
            "DELETE /api/proxy-sign-ins/{proxy} Install",
            "DELETE /api/rule-sets/{id} Own",
            "GET /api/identities/{id} Own",
            "POST /api/orphan-volumes/{id}/restore Install",
            "PUT /api/consents/{kind} Install",
            "PUT /api/env/{name} Own",
            "PUT /api/identities/{id} Own",
            "PUT /api/identities/{id}/default Own",
            "PUT /api/rule-sets/{id} Own",
            "PUT /api/rule-sets/{id}/switch Own",
        ]
    );
}

/// The reads across every user's data take the install's `AllUsers`, which the host takes before
/// it serves and never hands to the API. This crate's own code (its request handlers, the route
/// layer, the event stream) names none of it, so a handler cannot read past its principal's
/// workspaces by accident: it would have to take the capability on purpose, here, in review.
#[test]
fn no_request_handler_can_reach_the_reads_across_every_user() {
    fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                // Unit tests live in `tests/` folders and `tests.rs` files.
                if name != "tests" {
                    sources(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "rs") && name != "tests.rs" {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(files.len() > 50, "the walk found {} files", files.len());
    let mut found = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            for word in [
                "AllUsers",
                "take_all_users",
                "_every_user",
                "puddle_store::testing",
            ] {
                if line.contains(word) {
                    found.push(format!("{}:{}: {word}", file.display(), n + 1));
                }
            }
        }
    }
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn owner_only_actions_and_the_install_token_rule() {
    for action in [
        Action::ReadInstall,
        Action::ChangeInstall,
        Action::UseComputerAccounts,
        Action::ManageUsers,
        Action::SetRemoteServing,
    ] {
        assert!(action.owner_only(), "{action:?}");
    }
    // A user manages their own collections.
    for action in [
        Action::ReadService,
        Action::ReadWorkspace,
        Action::DecideRequests,
        Action::ReadAudit,
        Action::CreateWorkspace,
        Action::ReadWorkspaceGit,
        Action::ChangeWorkspaceGit,
        Action::ReadGlobalRules,
        Action::ChangeGlobalRules,
        Action::ReadIdentities,
        Action::ChangeIdentities,
        Action::ReadGlobalEnvironment,
        Action::ChangeGlobalEnvironment,
        Action::UseCredentials,
    ] {
        assert!(!action.owner_only(), "{action:?}");
    }
    assert!(Action::SetRemoteServing.needs_install_token());
    assert!(!Action::ManageUsers.needs_install_token());
}

fn store() -> Arc<Store> {
    Arc::new(Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap())
}

fn ws(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

#[test]
fn the_owner_alone_does_install_things_and_only_with_the_token_widens_reach() {
    let store = store();
    let ann = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    let isolation = Isolation::new(store);
    let owner = Principal::INSTALL;
    let owner_in_a_browser = Principal {
        via: Via::Device(DeviceId::from_stored(4)),
        ..owner
    };
    let ann = ann_principal(ann);
    let ok = |p: &Principal, a: Action, r: ResourceRef| isolation.check(p, a, &r).is_ok();
    assert!(ok(&owner, Action::ChangeInstall, ResourceRef::Install));
    assert!(ok(&owner, Action::SetRemoteServing, ResourceRef::Install));
    assert!(matches!(
        isolation.check(
            &owner_in_a_browser,
            Action::SetRemoteServing,
            &ResourceRef::Install
        ),
        Err(Denial::InstallTokenOnly)
    ));
    assert!(ok(
        &owner_in_a_browser,
        Action::ManageUsers,
        ResourceRef::Install
    ));
    for (action, resource) in [
        (Action::ReadInstall, ResourceRef::Install),
        (Action::ReadService, ResourceRef::Install),
        (Action::ManageUsers, ResourceRef::Own),
        (Action::UseComputerAccounts, ResourceRef::Install),
    ] {
        assert!(
            matches!(
                isolation.check(&ann, action, &resource),
                Err(Denial::OwnerOnly)
            ),
            "{action:?} {resource:?}"
        );
    }
    assert!(ok(&ann, Action::ReadService, ResourceRef::Service));
    assert!(ok(&ann, Action::ListWorkspaces, ResourceRef::Own));
    assert!(ok(&ann, Action::ChangeGlobalRules, ResourceRef::Own));
    assert!(ok(&ann, Action::CreateWorkspace, ResourceRef::Own));
}

#[test]
fn a_users_resources_are_theirs_alone_and_the_owner_has_no_way_in() {
    let store = store();
    let ann = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    store.set_workspace_owner(&ws("anns"), ann).unwrap();
    store
        .set_workspace_owner(&ws("owners"), UserId::OWNER)
        .unwrap();
    let pending = |workspace: &str| {
        let decision = store
            .decide(
                &EgressRequest::new(
                    ws(workspace),
                    Host::parse_normalised("a.example").unwrap(),
                    443,
                ),
                SuffixAllows::Count,
            )
            .unwrap();
        match decision {
            puddle_types::Decision::Pending(outcome) => outcome.pending_id().unwrap(),
            other => panic!("{other:?}"),
        }
    };
    let anns_request = pending("anns");
    let owners_request = pending("owners");
    let isolation = Isolation::new(store);
    let ann = ann_principal(ann);
    let owner = Principal::INSTALL;
    let check = |p: &Principal, r: ResourceRef| isolation.check(p, Action::ReadWorkspace, &r);
    assert!(check(&ann, ResourceRef::Workspace(ws("anns"))).is_ok());
    assert!(check(&ann, ResourceRef::Pending(anns_request)).is_ok());
    assert!(check(&owner, ResourceRef::Workspace(ws("owners"))).is_ok());
    assert!(check(&owner, ResourceRef::Pending(owners_request)).is_ok());
    // Another user's, and an id nobody has, answer alike for a user.
    let body = |r: Result<(), Denial>| match r {
        Err(Denial::NotFound(err)) => format!("{err:?}"),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        body(check(&ann, ResourceRef::Workspace(ws("owners")))).replace("owners", "x"),
        body(check(&ann, ResourceRef::Workspace(ws("nobody")))).replace("nobody", "x"),
    );
    assert!(matches!(
        check(&owner, ResourceRef::Workspace(ws("anns"))),
        Err(Denial::NotFound(_))
    ));
    assert!(matches!(
        check(&owner, ResourceRef::Pending(anns_request)),
        Err(Denial::NotFound(_))
    ));
    assert!(matches!(
        check(&ann, ResourceRef::Pending(PendingId(9_999))),
        Err(Denial::NotFound(_))
    ));
    // An id nobody has, and a workspace with no recorded user, are nobody's: the owner gets no
    // more of them than anyone else. (Such a workspace has no requests: the store decides none.)
    for principal in [&owner, &ann] {
        for resource in [
            ResourceRef::Workspace(ws("nobodys")),
            ResourceRef::Pending(PendingId(9_999)),
            ResourceRef::Rule(RuleId(9_999)),
            ResourceRef::RequestRule(9_999),
        ] {
            assert!(
                matches!(check(principal, resource.clone()), Err(Denial::NotFound(_))),
                "{principal:?} {resource:?}"
            );
        }
    }
}

/// A request the guard put no principal on is refused like a missing token.
#[tokio::test]
async fn a_request_without_a_principal_is_unauthorized() {
    let (mut parts, ()) = axum::http::Request::new(()).into_parts();
    let refused = Principal::from_request_parts(&mut parts, &())
        .await
        .unwrap_err();
    assert_eq!(refused.into_response().status(), StatusCode::UNAUTHORIZED);
    parts.extensions.insert(Principal::INSTALL);
    let found = Principal::from_request_parts(&mut parts, &())
        .await
        .unwrap();
    assert_eq!(found, Principal::INSTALL);
}

#[test]
fn a_denial_becomes_its_status() {
    for denial in [Denial::OwnerOnly, Denial::InstallTokenOnly] {
        assert_eq!(
            ApiError::from(denial).into_response().status(),
            StatusCode::FORBIDDEN
        );
    }
    let failed = ApiError::from(Denial::Failed(StoreError::InviteInvalid)).into_response();
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// Two servers on one set of services: one the owner calls, one Ann calls.
struct Pair {
    owner: (SocketAddr, RunningApi),
    ann: (SocketAddr, RunningApi),
    token: String,
    store: Arc<Store>,
    problems: Arc<puddle_types::Problems>,
    learned: Arc<FakeLearned>,
    ann_id: UserId,
}

async fn pair() -> Pair {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let events = Arc::new(EventHub::default());
    let store = Arc::new(
        Store::open_in_memory(clock.clone(), Limits::default())
            .unwrap()
            .with_events(events.clone()),
    );
    let ann_id = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    // The owner's workspace that the tests send requests from without creating it.
    puddle_store::testing::own(&store, &ws("ownerbox"));
    let services = Services::new(
        store.clone(),
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        events.clone(),
        clock.clone(),
    )
    .with_workspaces(Arc::new(FakeWorkspaces::new(events, clock)))
    .with_credentials(Arc::new(FakeCredentials::new()))
    .with_secret_store(Arc::new(MemoryStore::new()));
    let learned = Arc::new(FakeLearned::new());
    let services = services.with_learned(learned.clone());
    let problems = services.problems.clone();
    let token = ApiToken::generate().unwrap();
    let start = |principal| {
        let services = services.clone();
        let token = token.clone();
        async move {
            let server = ApiServer::bind_as(ApiConfig::default(), token, services, principal)
                .await
                .unwrap();
            (server.local_addr(), server.spawn())
        }
    };
    Pair {
        owner: start(Principal::INSTALL).await,
        ann: start(ann_principal(ann_id)).await,
        token: token.expose().to_owned(),
        store,
        problems,
        learned,
        ann_id,
    }
}

async fn send(
    addr: SocketAddr,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, String) {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n",
        addr.port()
    );
    if matches!(method, "POST" | "PUT" | "PATCH") {
        let _ = write!(
            request,
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        );
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(TOKEN_STEP, stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_owned();
    (status, body)
}

/// `path` with the resource's parameter set to `resource` and every other one to `1` (the check
/// answers before any handler reads them).
fn fill(path: &str, resource: &str) -> String {
    path.split('/')
        .map(|part| match part {
            "{id}" | "{workspace}" => resource,
            p if p.starts_with('{') => "1",
            p => p,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// What the owner has: a workspace, an open request of it, a rule and a rule on method and path
/// for it, and of their own collections an identity, a rule set, a global rule, a global
/// variable and a pasted token.
struct OwnersThings {
    workspace: String,
    pending: i64,
    rule: i64,
    request_rule: i64,
    identity: i64,
    rule_set: String,
    global_rule: i64,
    global_request_rule: i64,
    token: String,
}

/// A pasted token for the fake credential service, made at run time so no token-shaped literal is
/// in the source.
fn made_up_token(whose: &str) -> String {
    format!("ghp_{whose}{}", "0123456789")
}

fn identity_body(label: &str, source: &Value) -> Value {
    json!({
        "label": label,
        "author": {"name": label, "email": "me@example.com"},
        "credentials": [{
            "host": "github.com",
            "source": source,
            "covers": {"owners": ["acme"], "rest_of_host": false}
        }]
    })
}

/// Makes `what` at `path` from `addr` and gives back the answer's `id` (or `source.id`).
async fn make(addr: SocketAddr, token: &str, path: &str, what: &Value) -> Value {
    let method = if path.starts_with("/api/env/") {
        "PUT"
    } else {
        "POST"
    };
    let (status, body) = send(addr, token, method, path, Some(what)).await;
    assert!(status == 200 || status == 201, "{path}: {status} {body}");
    let made: Value = serde_json::from_str(&body).unwrap();
    if made["source"]["id"].is_string() {
        made["source"]["id"].clone()
    } else {
        made["id"].clone()
    }
}

async fn owners_things(pair: &Pair) -> OwnersThings {
    let (owner, token) = (pair.owner.0, pair.token.as_str());
    let id = |body: &str| serde_json::from_str::<Value>(body).unwrap()["id"].clone();
    let (status, created) = send(
        owner,
        token,
        "POST",
        "/api/workspaces",
        Some(&json!({"name": "box"})),
    )
    .await;
    assert_eq!(status, 202, "{created}");
    let workspace = id(&created).as_str().unwrap().to_owned();
    let request = EgressRequest::new(
        ws(&workspace),
        Host::parse_normalised("a.example").unwrap(),
        443,
    );
    let pending = match pair.store.decide(&request, SuffixAllows::Count).unwrap() {
        puddle_types::Decision::Pending(outcome) => outcome.pending_id().unwrap().0,
        other => panic!("{other:?}"),
    };
    let rule = json!({"scope": {"type": "workspace", "workspace": workspace}, "pattern": "b.example", "effect": "allow"});
    let (status, rule) = send(owner, token, "POST", "/api/rules", Some(&rule)).await;
    assert_eq!(status, 201, "{rule}");
    let request_rule =
        json!({"workspace": workspace, "host": "c.example", "methods": ["POST"], "path": "/x"});
    let (status, request_rule) = send(
        owner,
        token,
        "POST",
        "/api/request-rules",
        Some(&request_rule),
    )
    .await;
    assert_eq!(status, 201, "{request_rule}");
    let token_source = make(
        owner,
        token,
        "/api/credentials/stored",
        &json!({"host": "github.com", "token": made_up_token("owners")}),
    )
    .await;
    let stored = json!({"kind": "stored", "id": token_source, "host": "github.com"});
    OwnersThings {
        pending,
        rule: id(&rule).as_i64().unwrap(),
        request_rule: id(&request_rule).as_i64().unwrap(),
        workspace,
        identity: make(
            owner,
            token,
            "/api/identities",
            &identity_body("Owners", &stored),
        )
        .await
        .as_i64()
        .unwrap(),
        rule_set: make(
            owner,
            token,
            "/api/rule-sets",
            &json!({"name": "Owners set", "description": ""}),
        )
        .await
        .as_str()
        .unwrap()
        .to_owned(),
        global_rule: make(
            owner,
            token,
            "/api/rules",
            &json!({"scope": {"type": "global"}, "pattern": "owners.example", "effect": "allow"}),
        )
        .await
        .as_i64()
        .unwrap(),
        global_request_rule: make(
            owner,
            token,
            "/api/request-rules",
            &json!({"host": "owners.example", "methods": ["DELETE"], "path": "/x"}),
        )
        .await
        .as_i64()
        .unwrap(),
        token: {
            make(
                owner,
                token,
                "/api/env/OWNERS_ONLY",
                &json!({"kind": "plain", "value": "owners-value"}),
            )
            .await;
            token_source.as_str().unwrap().to_owned()
        },
    }
}

/// From Ann's session, every route that names a resource answers for the owner's
/// workspace, request and rules exactly as for ids nobody has, and never with the resource. The
/// owner, in turn, gets nothing of Ann's. Lists keep to their caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn hostile_every_route_keeps_each_user_to_their_own() {
    let pair = pair().await;
    let (owner, ann) = (pair.owner.0, pair.ann.0);
    let token = pair.token.as_str();
    let OwnersThings {
        workspace: box_id,
        pending,
        rule: rule_id,
        request_rule: request_rule_id,
        identity,
        rule_set,
        global_rule,
        global_request_rule,
        token: stored_token,
    } = owners_things(&pair).await;

    let mut checked = 0;
    for (method, path) in operations() {
        let declared = declaration(&method, &path).unwrap();
        let (foreign, unknown) = match declared.target {
            Target::Workspace(_) => (box_id.clone(), "nobody00".to_owned()),
            Target::Pending => (pending.to_string(), "999999".to_owned()),
            Target::Rule => (rule_id.to_string(), "999999".to_owned()),
            Target::RequestRule => (request_rule_id.to_string(), "999999".to_owned()),
            Target::Ticket | Target::Service | Target::Own | Target::Install => continue,
        };
        let body = matches!(method, Method::POST | Method::PUT | Method::PATCH).then(|| json!({}));
        let m = method.as_str();
        let theirs = send(ann, token, m, &fill(&path, &foreign), body.as_ref()).await;
        let nobodys = send(ann, token, m, &fill(&path, &unknown), body.as_ref()).await;
        assert_eq!(
            (theirs.0, theirs.1.replace(&foreign, "ID")),
            (nobodys.0, nobodys.1.replace(&unknown, "ID")),
            "{m} {path}"
        );
        let want = if declared.action.owner_only() {
            403
        } else {
            404
        };
        assert_eq!(theirs.0, want, "{m} {path}: {}", theirs.1);
        checked += 1;
    }
    assert_eq!(checked, 53, "routes that name a resource");

    // The routes of a user's own collections name the resource by an id the store looks up for
    // the principal: the owner's answer exactly as ids nobody has.
    let user_set = |id: &str| id.to_owned();
    let by_id: Vec<(&str, String, String, Option<Value>)> = vec![
        (
            "GET",
            format!("/api/identities/{identity}"),
            "/api/identities/999999".into(),
            None,
        ),
        (
            "PUT",
            format!("/api/identities/{identity}"),
            "/api/identities/999999".into(),
            Some(identity_body(
                "Mine",
                &json!({"kind": "stored", "id": "u2-tok-x", "host": "github.com"}),
            )),
        ),
        (
            "DELETE",
            format!("/api/identities/{identity}"),
            "/api/identities/999999".into(),
            None,
        ),
        (
            "PUT",
            format!("/api/identities/{identity}/default"),
            "/api/identities/999999/default".into(),
            None,
        ),
        (
            "PUT",
            format!("/api/rule-sets/{}", user_set(&rule_set)),
            "/api/rule-sets/user:999999".into(),
            Some(json!({"name": "x", "description": ""})),
        ),
        (
            "DELETE",
            format!("/api/rule-sets/{rule_set}"),
            "/api/rule-sets/user:999999".into(),
            None,
        ),
        (
            "PUT",
            format!("/api/rule-sets/{rule_set}/switch"),
            "/api/rule-sets/user:999999/switch".into(),
            Some(json!({"enabled": false})),
        ),
        (
            "DELETE",
            format!("/api/rules/{global_rule}"),
            "/api/rules/999999".into(),
            None,
        ),
        (
            "PUT",
            format!("/api/rules/{global_rule}/expiry"),
            "/api/rules/999999/expiry".into(),
            Some(json!({"expires_at": null})),
        ),
        (
            "DELETE",
            format!("/api/request-rules/{global_request_rule}"),
            "/api/request-rules/999999".into(),
            None,
        ),
        (
            "DELETE",
            "/api/env/OWNERS_ONLY".into(),
            "/api/env/NOBODYS".into(),
            None,
        ),
    ];
    for (method, theirs_path, nobodys_path, body) in by_id {
        let theirs = send(ann, token, method, &theirs_path, body.as_ref()).await;
        let nobodys = send(ann, token, method, &nobodys_path, body.as_ref()).await;
        let id_of = |path: &str| {
            path.rsplit('/')
                .find(|p| {
                    p.chars().any(|c| c.is_ascii_digit())
                        || p.contains("ONLY")
                        || p.contains("NOBODY")
                })
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!(
            (theirs.0, theirs.1.replace(&id_of(&theirs_path), "ID")),
            (nobodys.0, nobodys.1.replace(&id_of(&nobodys_path), "ID")),
            "{method} {theirs_path}"
        );
        assert_eq!(theirs.0, 404, "{method} {theirs_path}: {}", theirs.1);
    }
    // Rule sets: a switch for the owner's workspace is refused as an unknown workspace.
    let (status, body) = send(
        ann,
        token,
        "PUT",
        "/api/rule-sets/builtin:github/switch",
        Some(&json!({"workspace": box_id, "enabled": true})),
    )
    .await;
    assert_eq!(status, 404, "{body}");
    // A pasted token of the owner's: refused the same as one made up, never removed.
    let theirs_source = json!({"kind": "stored", "id": stored_token, "host": "github.com"});
    let made_up = json!({"kind": "stored", "id": "tok-0000", "host": "github.com"});
    let made_up_of_ann =
        json!({"kind": "stored", "id": format!("u{}-tok-0000", pair.ann_id), "host": "github.com"});
    for path in ["/api/credentials/check", "/api/credentials/profile"] {
        let theirs = send(
            ann,
            token,
            "POST",
            path,
            Some(&json!({"source": theirs_source})),
        )
        .await;
        let nobodys = send(ann, token, "POST", path, Some(&json!({"source": made_up}))).await;
        assert_eq!(theirs, nobodys, "{path}");
        assert_eq!(theirs.0, 422, "{path}: {}", theirs.1);
        let own = send(
            ann,
            token,
            "POST",
            path,
            Some(&json!({"source": made_up_of_ann})),
        )
        .await;
        assert_ne!(own.0, 422, "{path}: {}", own.1);
    }
    let (status, body) = send(
        ann,
        token,
        "POST",
        "/api/identities",
        Some(&identity_body("Theirs", &theirs_source)),
    )
    .await;
    assert_eq!(status, 422, "{body}");
    let (status, _) = send(
        ann,
        token,
        "DELETE",
        &format!("/api/credentials/stored/{stored_token}"),
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (_, owners_view) = send(
        owner,
        token,
        "GET",
        &format!("/api/identities/{identity}"),
        None,
    )
    .await;
    assert!(owners_view.contains(&stored_token), "{owners_view}");
    // The accounts signed in on the computer are the owner's.
    let gh = json!({"kind": "gh", "host": "github.com", "account": "me"});
    for (method, path, body) in [
        ("GET", "/api/credentials/found", None),
        (
            "POST",
            "/api/credentials/sign-in",
            Some(json!({"source": gh})),
        ),
        (
            "POST",
            "/api/credentials/check",
            Some(json!({"source": gh})),
        ),
        ("POST", "/api/identities", Some(identity_body("Gh", &gh))),
    ] {
        let (status, answer) = send(ann, token, method, path, body.as_ref()).await;
        assert_eq!(status, 403, "{method} {path}: {answer}");
    }

    pair.problems
        .raise(puddle_types::Problem::new("ws", "t", "d").about(ws(&box_id)));
    pair.problems
        .raise(puddle_types::Problem::new("puddle", "own", "d"));
    // Ann's lists hold nothing of the owner's: every list there is, read from the route table so
    // a new one is checked too (the event stream has its own test; the lookup is below).
    let get = |path: &'static str| async move { send(ann, token, "GET", path, None).await };
    let lists: Vec<String> = operations()
        .into_iter()
        .filter(|(method, path)| {
            *method == Method::GET
                && !path.contains('{')
                && !matches!(path.as_str(), "/api/events" | "/api/workspaces/lookup")
                && declaration(method, path).unwrap().target == Target::Own
        })
        .map(|(_, path)| path)
        .collect();
    assert_eq!(lists.len(), 13, "{lists:?}");
    for path in &lists {
        let (status, body) = send(ann, token, "GET", path, None).await;
        // The devices list answers 503 here: this server has no sign-in.
        assert!(matches!(status, 200 | 503), "{path}: {status} {body}");
        for owners in [
            box_id.as_str(),
            "Owners",
            "owners.example",
            "OWNERS_ONLY",
            &stored_token,
        ] {
            assert!(!body.contains(owners), "{path} holds {owners}: {body}");
        }
    }
    let (status, _) = get("/api/workspaces/lookup?q=box").await;
    assert_eq!(status, 404);
    // The owner's do (the control).
    for path in [
        "/api/workspaces",
        "/api/pending",
        "/api/rules",
        "/api/request-rules",
        "/api/audit",
        "/api/problems",
    ] {
        let (_, body) = send(owner, token, "GET", path, None).await;
        assert!(body.contains(&box_id), "{path}: {body}");
    }

    // A rule for the owner's workspace is refused as an unknown workspace.
    let theirs = json!({"scope": {"type": "workspace", "workspace": box_id}, "pattern": "d.example", "effect": "allow"});
    assert_eq!(
        send(ann, token, "POST", "/api/rules", Some(&theirs))
            .await
            .0,
        404
    );
    let theirs = json!({"workspace": box_id, "host": "c.example", "methods": [], "path": "/y"});
    assert_eq!(
        send(ann, token, "POST", "/api/request-rules", Some(&theirs))
            .await
            .0,
        404
    );
    pair.owner.1.shutdown().await;
    pair.ann.1.shutdown().await;
}

/// Ann may make rules for, and decide the requests of, a workspace of her own; a decision for
/// every workspace makes a global rule of hers, which decides none of the owner's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_decides_for_their_own_workspace_only() {
    let pair = pair().await;
    let (ann, token) = (pair.ann.0, pair.token.as_str());
    let mine = ws("annsbox1");
    pair.store.set_workspace_owner(&mine, pair.ann_id).unwrap();
    let ask = |host: &str| match pair
        .store
        .decide(
            &EgressRequest::new(mine.clone(), Host::parse_normalised(host).unwrap(), 443),
            SuffixAllows::Count,
        )
        .unwrap()
    {
        puddle_types::Decision::Pending(outcome) => outcome.pending_id().unwrap().0,
        other => panic!("{other:?}"),
    };
    let first = ask("a.example");
    let second = ask("b.example");
    let (status, list) = send(ann, token, "GET", "/api/pending", None).await;
    assert_eq!(status, 200);
    assert!(
        list.contains("a.example") && list.contains("b.example"),
        "{list}"
    );
    let (status, inbox) = send(ann, token, "GET", "/api/inbox", None).await;
    assert_eq!(status, 200);
    assert!(inbox.contains("a.example"), "{inbox}");
    let everywhere = json!({"scope": "global"});
    let (status, body) = send(
        ann,
        token,
        "POST",
        &format!("/api/pending/{first}/approve"),
        Some(&everywhere),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let request = |workspace: &str| {
        EgressRequest::new(
            ws(workspace),
            Host::parse_normalised("a.example").unwrap(),
            443,
        )
    };
    assert!(matches!(
        pair.store
            .decide(&request("annsbox1"), SuffixAllows::Count)
            .unwrap(),
        puddle_types::Decision::Allow { .. }
    ));
    assert!(matches!(
        pair.store
            .decide(&request("ownerbox"), SuffixAllows::Count)
            .unwrap(),
        puddle_types::Decision::Pending(_)
    ));
    let (status, body) = send(
        ann,
        token,
        "POST",
        &format!("/api/pending/{second}/deny"),
        Some(&json!({})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let own = json!({"scope": {"type": "workspace", "workspace": "annsbox1"}, "pattern": "e.example", "effect": "deny"});
    let (status, body) = send(ann, token, "POST", "/api/rules", Some(&own)).await;
    assert_eq!(status, 201, "{body}");
    let (_, rules) = send(ann, token, "GET", "/api/rules", None).await;
    assert!(
        rules.contains("e.example") && rules.contains("b.example"),
        "{rules}"
    );
    pair.owner.1.shutdown().await;
    pair.ann.1.shutdown().await;
}

/// The other way round: a workspace Ann made is out of the owner's reach on every route, answered
/// exactly as one nobody has, while Ann reaches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_the_owner_has_no_way_into_a_users_workspace() {
    let pair = pair().await;
    let (owner, ann, token) = (pair.owner.0, pair.ann.0, pair.token.as_str());
    let (status, created) = send(
        ann,
        token,
        "POST",
        "/api/workspaces",
        Some(&json!({"name": "anns"})),
    )
    .await;
    assert_eq!(status, 202, "{created}");
    let anns = serde_json::from_str::<Value>(&created).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = send(ann, token, "GET", &format!("/api/workspaces/{anns}"), None).await;
    assert_eq!(status, 200, "Ann reaches her own: {body}");
    let mut checked = 0;
    for (method, path) in operations() {
        if !matches!(
            declaration(&method, &path).unwrap().target,
            Target::Workspace(_)
        ) {
            continue;
        }
        let body = matches!(method, Method::POST | Method::PUT | Method::PATCH).then(|| json!({}));
        let m = method.as_str();
        let theirs = send(owner, token, m, &fill(&path, &anns), body.as_ref()).await;
        let nobodys = send(owner, token, m, &fill(&path, "nobody00"), body.as_ref()).await;
        assert_eq!(theirs.0, 404, "{m} {path}: {}", theirs.1);
        assert_eq!(
            theirs.1.replace(&anns, "ID"),
            nobodys.1.replace("nobody00", "ID"),
            "{m} {path}"
        );
        checked += 1;
    }
    assert!(checked > 40, "{checked} routes name a workspace");
    let (_, listed) = send(owner, token, "GET", "/api/workspaces", None).await;
    assert!(!listed.contains(&anns), "{listed}");
    pair.owner.1.shutdown().await;
    pair.ann.1.shutdown().await;
}

/// Every workspace's never-decrypt list is the install's: a user may put a learned host of her
/// workspace on its own list, not on the one for every workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_makes_a_learned_host_permanent_for_her_workspace_only() {
    let pair = pair().await;
    let (owner, ann, token) = (pair.owner.0, pair.ann.0, pair.token.as_str());
    let (status, created) = send(
        ann,
        token,
        "POST",
        "/api/workspaces",
        Some(&json!({"name": "anns"})),
    )
    .await;
    assert_eq!(status, 202, "{created}");
    let anns = ws(serde_json::from_str::<Value>(&created).unwrap()["id"]
        .as_str()
        .unwrap());
    for host in ["one.example", "two.example"] {
        pair.learned.learn(&anns, host, 600);
    }
    let permanent = |host: &str, scope: &str| {
        let path = format!("/api/workspaces/{anns}/interception/learned/{host}/permanent");
        let body = json!({"scope": scope});
        async move { send(ann, token, "POST", &path, Some(&body)).await }
    };
    let (status, body) = permanent("one.example", "global").await;
    assert_eq!(status, 403, "{body}");
    assert!(
        pair.store.global_never_decrypt().unwrap().is_empty(),
        "nothing changed"
    );
    let still = pair.learned.list(&anns);
    assert_eq!(still.len(), 2, "still learned: {still:?}");
    let (status, body) = permanent("one.example", "workspace").await;
    assert_eq!(status, 204, "{body}");
    assert_eq!(
        pair.store
            .workspace_interception(&anns)
            .unwrap()
            .never_decrypt,
        ["one.example"]
    );
    // The owner may, for a workspace of their own.
    let (status, created) = send(
        owner,
        token,
        "POST",
        "/api/workspaces",
        Some(&json!({"name": "box"})),
    )
    .await;
    assert_eq!(status, 202, "{created}");
    let boxed = ws(serde_json::from_str::<Value>(&created).unwrap()["id"]
        .as_str()
        .unwrap());
    pair.learned.learn(&boxed, "three.example", 600);
    let (status, body) = send(
        owner,
        token,
        "POST",
        &format!("/api/workspaces/{boxed}/interception/learned/three.example/permanent"),
        Some(&json!({"scope": "global"})),
    )
    .await;
    assert_eq!(status, 204, "{body}");
    assert_eq!(
        pair.store.global_never_decrypt().unwrap(),
        ["three.example"]
    );
    pair.owner.1.shutdown().await;
    pair.ann.1.shutdown().await;
}

/// Puddle's own problems are the owner's, the install's routes stay the owner's, an id that does
/// not parse is unknown, and a `HEAD` is checked as its `GET`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn odd_requests_meet_the_same_checks() {
    let pair = pair().await;
    let (owner, ann, token) = (pair.owner.0, pair.ann.0, pair.token.as_str());
    pair.problems
        .raise(puddle_types::Problem::new("puddle", "own", "d"));
    let (_, problems) = send(ann, token, "GET", "/api/problems", None).await;
    assert_eq!(problems, r#"{"problems":[]}"#);
    let (_, problems) = send(owner, token, "GET", "/api/problems", None).await;
    assert!(problems.contains("puddle"), "{problems}");
    let (status, _) = send(ann, token, "GET", "/api/orphan-volumes", None).await;
    assert_eq!(status, 403);
    // An id that does not parse names nothing: the same answer as an unknown one.
    let (status, _) = send(ann, token, "GET", "/api/workspaces/NOT%20AN%20ID/git", None).await;
    assert_eq!(status, 404);
    assert_eq!(
        send(owner, token, "HEAD", "/api/workspaces", None).await.0,
        200
    );
    pair.owner.1.shutdown().await;
    pair.ann.1.shutdown().await;
}

/// A user's own collections work end to end from their session, and nothing in them reaches the
/// owner: a global rule, a rule set, a global variable, an identity on a pasted token of theirs,
/// a workspace they make (recorded as theirs before it is listed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn a_user_manages_their_own_collections() {
    let pair = pair().await;
    let (owner, ann, token) = (pair.owner.0, pair.ann.0, pair.token.as_str());
    let (status, created) = send(
        ann,
        token,
        "POST",
        "/api/workspaces",
        Some(&json!({"name": "anns"})),
    )
    .await;
    assert_eq!(status, 202, "{created}");
    let anns: Value = serde_json::from_str(&created).unwrap();
    let anns = ws(anns["id"].as_str().unwrap());
    assert_eq!(
        pair.store.workspace_owner(&anns).unwrap(),
        Some(pair.ann_id)
    );
    let (_, listed) = send(owner, token, "GET", "/api/workspaces", None).await;
    assert!(!listed.contains(anns.as_str()), "{listed}");

    let rule = make(
        ann,
        token,
        "/api/rules",
        &json!({"scope": {"type": "global"}, "pattern": "anns.example", "effect": "allow"}),
    )
    .await;
    let set = make(
        ann,
        token,
        "/api/rule-sets",
        &json!({"name": "Mine", "description": ""}),
    )
    .await;
    make(
        ann,
        token,
        "/api/env/ANNS",
        &json!({"kind": "secret", "value": "anns-secret", "hosts": ["anns.example"]}),
    )
    .await;
    let source = make(
        ann,
        token,
        "/api/credentials/stored",
        &json!({"host": "github.com", "token": made_up_token("anns")}),
    )
    .await;
    let source = source.as_str().unwrap().to_owned();
    assert!(
        source.starts_with(&format!("u{}-tok-", pair.ann_id)),
        "{source}"
    );
    let identity = make(
        ann,
        token,
        "/api/identities",
        &identity_body(
            "Anns",
            &json!({"kind": "stored", "id": source, "host": "github.com"}),
        ),
    )
    .await;
    let (status, body) = send(
        ann,
        token,
        "PUT",
        &format!("/api/workspaces/{anns}/identities"),
        Some(&json!({"ids": [identity]})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = send(
        ann,
        token,
        "PUT",
        "/api/identities/git-defaults",
        Some(&json!({"only_push_listed": false})),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    // Ann's rule decides her workspace and not the owner's.
    let request = |workspace: &WorkspaceId| {
        EgressRequest::new(
            workspace.clone(),
            Host::parse_normalised("anns.example").unwrap(),
            443,
        )
    };
    assert!(matches!(
        pair.store
            .decide(&request(&anns), SuffixAllows::Count)
            .unwrap(),
        puddle_types::Decision::Allow { .. }
    ));
    assert!(matches!(
        pair.store
            .decide(&request(&ws("ownerbox")), SuffixAllows::Count)
            .unwrap(),
        puddle_types::Decision::Pending(_)
    ));
    // Her variable's secret is under her name in the credential store.
    let secret = pair
        .store
        .env_entries(pair.ann_id, &puddle_store::EnvScope::Global)
        .unwrap();
    let marked = secret[0]
        .value
        .as_secret()
        .unwrap()
        .id
        .as_str()
        .starts_with(&format!("u{}-env-", pair.ann_id));
    assert!(marked, "the secret's entry name carries Ann's id");

    // The owner sees none of it.
    for path in [
        "/api/rules",
        "/api/rule-sets",
        "/api/env",
        "/api/identities",
    ] {
        let (_, body) = send(owner, token, "GET", path, None).await;
        for anns_own in ["anns.example", "Mine", "ANNS", "Anns", source.as_str()] {
            assert!(!body.contains(anns_own), "{path} holds {anns_own}: {body}");
        }
    }
    let (_, defaults) = send(owner, token, "GET", "/api/identities/git-defaults", None).await;
    assert!(
        defaults.contains(r#""only_push_listed":true"#),
        "{defaults}"
    );
    for path in [
        format!("/api/rules/{rule}"),
        format!("/api/rule-sets/{}", set.as_str().unwrap()),
        format!("/api/identities/{identity}"),
        "/api/env/ANNS".to_owned(),
    ] {
        let (status, body) = send(owner, token, "DELETE", &path, None).await;
        assert_eq!(status, 404, "{path}: {body}");
    }
    // Her audit holds her records; the owner's does not.
    let (_, audit) = send(ann, token, "GET", "/api/audit", None).await;
    assert!(audit.contains("anns.example"), "{audit}");
    let (_, audit) = send(owner, token, "GET", "/api/audit", None).await;
    assert!(!audit.contains(r#""pattern":"anns.example""#), "{audit}");
    assert!(
        !audit.contains(&format!(r#""workspace_id":"{anns}""#)),
        "{audit}"
    );
    pair.owner.1.shutdown().await;
    pair.ann.1.shutdown().await;
}
