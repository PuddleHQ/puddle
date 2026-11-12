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
use crate::server::{ApiConfig, ApiServer, RunningApi, Services};
use crate::settings::{MemorySettings, SettingsRepo};
use crate::token::ApiToken;
use crate::{FakeWorkspaces, openapi};

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
        125,
        "124 documented operations and the contract"
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

#[test]
fn owner_only_actions_and_the_install_token_rule() {
    for action in [
        Action::ReadInstall,
        Action::ChangeInstall,
        Action::ManageUsers,
        Action::SetRemoteServing,
        Action::CreateWorkspace,
        Action::ReadIdentities,
        Action::ReadGlobalRules,
    ] {
        assert!(action.owner_only(), "{action:?}");
    }
    for action in [
        Action::ReadService,
        Action::ReadWorkspace,
        Action::DecideRequests,
        Action::ReadAudit,
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
        (Action::ChangeGlobalRules, ResourceRef::Own),
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
}

#[test]
fn a_users_resources_are_theirs_alone_and_the_owner_has_no_way_in() {
    let store = store();
    let ann = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    store.set_workspace_owner(&ws("anns"), ann).unwrap();
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
    // Global rules are the owner's.
    assert!(matches!(
        check(&ann, ResourceRef::Rule(RuleId(9_999))),
        Err(Denial::NotFound(_))
    ));
    assert!(check(&owner, ResourceRef::RequestRule(9_999)).is_ok());
    assert!(matches!(
        check(&ann, ResourceRef::RequestRule(9_999)),
        Err(Denial::NotFound(_))
    ));
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
    let services = Services::new(
        store.clone(),
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        events.clone(),
        clock.clone(),
    )
    .with_workspaces(Arc::new(FakeWorkspaces::new(events, clock)));
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
/// for it.
struct OwnersThings {
    workspace: String,
    pending: i64,
    rule: i64,
    request_rule: i64,
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
    OwnersThings {
        pending,
        rule: id(&rule).as_i64().unwrap(),
        request_rule: id(&request_rule).as_i64().unwrap(),
        workspace,
    }
}

/// From Ann's session, every route that names a resource answers for the owner's
/// workspace, request and rules exactly as for ids nobody has, and never with the resource. The
/// owner, in turn, gets nothing of Ann's. Lists keep to their caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_every_route_keeps_each_user_to_their_own() {
    let pair = pair().await;
    let (owner, ann) = (pair.owner.0, pair.ann.0);
    let token = pair.token.as_str();
    let OwnersThings {
        workspace: box_id,
        pending,
        rule: rule_id,
        request_rule: request_rule_id,
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
    assert_eq!(checked, 50, "routes that name a resource");

    pair.problems
        .raise(puddle_types::Problem::new("ws", "t", "d").about(ws(&box_id)));
    pair.problems
        .raise(puddle_types::Problem::new("puddle", "own", "d"));
    // Ann's lists hold nothing of the owner's.
    let get = |path: &'static str| async move { send(ann, token, "GET", path, None).await };
    for path in [
        "/api/workspaces",
        "/api/pending",
        "/api/inbox",
        "/api/rules",
        "/api/request-rules",
        "/api/audit",
        "/api/problems",
    ] {
        let (status, body) = get(path).await;
        assert_eq!(status, 200, "{path}: {body}");
        assert!(!body.contains(&box_id), "{path}: {body}");
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

    // A rule that would reach past Ann's own workspace is refused, as is deciding a request
    // with one.
    let global = json!({"scope": {"type": "global"}, "pattern": "d.example", "effect": "allow"});
    assert_eq!(
        send(ann, token, "POST", "/api/rules", Some(&global))
            .await
            .0,
        403
    );
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

/// Ann may make rules for, and decide the requests of, a workspace of her own, but a decision
/// that would reach every workspace is the owner's.
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
    let (status, _) = send(
        ann,
        token,
        "POST",
        &format!("/api/pending/{first}/approve"),
        Some(&everywhere),
    )
    .await;
    assert_eq!(status, 403);
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

/// The other way round: a workspace of Ann's is out of the owner's reach on every route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_the_owner_has_no_way_into_a_users_workspace() {
    let pair = pair().await;
    let (owner, token) = (pair.owner.0, pair.token.as_str());
    pair.store
        .set_workspace_owner(&ws("annsbox1"), pair.ann_id)
        .unwrap();
    for (method, path) in operations() {
        if !matches!(
            declaration(&method, &path).unwrap().target,
            Target::Workspace(_)
        ) {
            continue;
        }
        let body = matches!(method, Method::POST | Method::PUT | Method::PATCH).then(|| json!({}));
        let (status, answer) = send(
            owner,
            token,
            method.as_str(),
            &fill(&path, "annsbox1"),
            body.as_ref(),
        )
        .await;
        assert_eq!(status, 404, "{method} {path}: {answer}");
    }
    pair.owner.1.shutdown().await;
    pair.ann.1.shutdown().await;
}

/// Puddle's own problems are the owner's, an id that does not parse still meets the role check,
/// and a `HEAD` is checked as its `GET`.
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
    let (status, _) = send(ann, token, "GET", "/api/workspaces/NOT%20AN%20ID/git", None).await;
    assert_eq!(status, 403);
    assert_eq!(
        send(owner, token, "HEAD", "/api/workspaces", None).await.0,
        200
    );
    pair.owner.1.shutdown().await;
    pair.ann.1.shutdown().await;
}
