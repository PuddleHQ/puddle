// SPDX-License-Identifier: AGPL-3.0-or-later
//! The browser editor under remote serving, over real HTTP: a request whose `Host` is a
//! workspace's name goes to the editor's entry instead of the API's routes, a name of no
//! workspace gets the same answer as a workspace's, and two legacy ids whose names clash raise a
//! problem.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use puddle_api::{
    ApiConfig, ApiServer, ApiToken, DisplayName, EventHub, FakeWorkspaces, MemorySettings, NoNames,
    PublicHost, Remote, RemoteServing, RunningApi, Services, SettingsRepo, Unsaved,
    WorkspaceRecord, WorkspaceService,
};
use puddle_store::{Limits, ManualClock, Store, UserId};
use puddle_types::WorkspaceId;

use crate::common::{Reply, START_MS, raw};

const UI: &str = "puddle.example.org";
const BASE: &str = "ws.example.net";
const WS: &str = "k3x9q7m2";

struct Site {
    running: RunningApi,
    addr: SocketAddr,
    install_token: String,
    workspaces: Arc<FakeWorkspaces>,
}

/// A site with remote serving on (or off) and the workspaces `seeded` (id and display name).
async fn site(remote: bool, seeded: &[(&str, &str)]) -> Site {
    let clock = Arc::new(ManualClock::new(START_MS));
    let events = Arc::new(EventHub::default());
    let store = Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap());
    let workspaces = Arc::new(FakeWorkspaces::new(events.clone(), clock.clone()));
    for (id, name) in seeded {
        let record = WorkspaceRecord::new(
            WorkspaceId::new(id).unwrap(),
            UserId::OWNER,
            DisplayName::parse(name).unwrap(),
            Vec::new(),
        );
        workspaces.seed(record, Unsaved::default());
        puddle_store::testing::own(&store, &WorkspaceId::new(id).unwrap());
    }
    let services = Services::new(
        store.clone(),
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        events,
        clock,
    )
    .with_front_resolver(Arc::new(NoNames))
    .with_workspaces(workspaces.clone() as Arc<dyn WorkspaceService>)
    .with_account_secrets(store.take_account_secrets().unwrap());
    let mut config = ApiConfig::default();
    if remote {
        config.remote = Some(Remote::new(
            RemoteServing::new(vec![PublicHost::parse(UI).unwrap()], Some(BASE)).unwrap(),
        ));
    }
    let token = ApiToken::generate().unwrap();
    let server = ApiServer::bind(config, token.clone(), services)
        .await
        .unwrap();
    let addr = server.local_addr();
    Site {
        running: server.spawn(),
        addr,
        install_token: token.expose().to_owned(),
        workspaces,
    }
}

impl Site {
    async fn get(&self, path: &str, host: &str, headers: &[(&str, &str)]) -> Reply {
        let mut text = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
        for (name, value) in headers {
            let _ = write!(text, "{name}: {value}\r\n");
        }
        text.push_str("\r\n");
        raw(self.addr, text.as_bytes()).await
    }

    fn loopback(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }

    async fn problems(&self) -> serde_json::Value {
        let auth = format!("Bearer {}", self.install_token);
        let reply = self
            .get(
                "/api/problems",
                &self.loopback(),
                &[("Authorization", &auth)],
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        reply.json()
    }
}

fn is_text(reply: &Reply) -> bool {
    reply
        .header("content-type")
        .is_some_and(|t| t.starts_with("text/plain"))
}

#[tokio::test]
async fn a_workspace_name_reaches_the_editor_entry_and_never_the_api_routes() {
    let site = site(true, &[(WS, "Shop")]).await;
    let auth = format!("Bearer {}", site.install_token);
    for host in [format!("{WS}.{BASE}"), format!("5173-{WS}.{BASE}")] {
        for path in ["/", "/api/health", "/api/workspaces"] {
            let reply = site.get(path, &host, &[("Authorization", &auth)]).await;
            assert_eq!(reply.status, 403, "{host}{path}: {reply:?}");
            assert!(is_text(&reply), "{host}{path}: {reply:?}");
            assert!(reply.body.contains("from Puddle"), "{reply:?}");
            assert_eq!(reply.header("cache-control"), Some("no-store"));
            assert_eq!(
                reply.header("content-security-policy"),
                Some("default-src 'none'; sandbox")
            );
        }
    }
    site.running.shutdown().await;
}

#[tokio::test]
async fn a_name_of_no_workspace_is_the_editors_and_other_names_stay_the_apis() {
    let site = site(true, &[(WS, "Shop")]).await;
    for host in [
        format!("abcdefgh.{BASE}"),
        format!("0-{WS}.{BASE}"),
        format!("70000-{WS}.{BASE}"),
        format!("5173-abcdefgh.{BASE}"),
    ] {
        let reply = site.get("/", &host, &[]).await;
        assert_eq!(reply.status, 403, "{host}: {reply:?}");
        assert!(is_text(&reply), "{host}: {reply:?}");
        assert!(reply.body.contains("from Puddle"), "{reply:?}");
    }
    // Deeper names, another port and the base domain itself are no workspace's: the API refuses
    // them as names that are not its own.
    for host in [
        format!("a.{WS}.{BASE}"),
        format!("{WS}.{BASE}:8443"),
        BASE.to_owned(),
    ] {
        let reply = site.get("/api/health", &host, &[]).await;
        assert_eq!(reply.status, 421, "{host}: {reply:?}");
        assert_eq!(reply.json()["error"], "misdirected_host", "{reply:?}");
    }
    site.running.shutdown().await;
}

#[tokio::test]
async fn with_remote_serving_off_a_workspace_name_is_the_apis_to_refuse() {
    let site = site(false, &[(WS, "Shop")]).await;
    let reply = site.get("/", &format!("{WS}.{BASE}"), &[]).await;
    assert_eq!(reply.status, 421, "{reply:?}");
    assert_eq!(reply.json()["error"], "misdirected_host", "{reply:?}");
    site.running.shutdown().await;
}

#[tokio::test]
async fn two_legacy_ids_whose_names_clash_raise_a_problem_and_the_editor_wins() {
    let site = site(true, &[("api", "Shop API"), ("8080-api", "Eight")]).await;
    let listed = site.problems().await;
    let problems = listed["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{listed}");
    assert_eq!(problems[0]["key"], "host-label-shadow:8080-api");
    assert_eq!(problems[0]["workspace"], "8080-api");
    // The clashing label is the editor of `8080-api`, not port 8080 of `api`.
    let reply = site.get("/", &format!("8080-api.{BASE}"), &[]).await;
    assert_eq!(reply.status, 403, "{reply:?}");
    // A rename shows in the problem from the next request under a workspace's name on.
    site.workspaces.seed(
        WorkspaceRecord::new(
            WorkspaceId::new("8080-api").unwrap(),
            UserId::OWNER,
            DisplayName::parse("Eighty").unwrap(),
            Vec::new(),
        ),
        Unsaved::default(),
    );
    site.get("/", &format!("api.{BASE}"), &[]).await;
    let listed = site.problems().await;
    assert_eq!(
        listed["problems"][0]["title"], "Eighty and Shop API share an address on this host",
        "{listed}"
    );
    site.running.shutdown().await;
}

#[tokio::test]
async fn a_caller_cannot_tell_a_workspace_id_from_a_name_of_none() {
    let site = site(true, &[(WS, "Shop")]).await;
    let known = site.get("/", &format!("{WS}.{BASE}"), &[]).await;
    for host in [
        format!("abcdefgh.{BASE}"),
        format!("5173-{WS}.{BASE}"),
        format!("5173-abcdefgh.{BASE}"),
        format!("0-{WS}.{BASE}"),
    ] {
        let reply = site.get("/", &host, &[]).await;
        assert_eq!(reply.status, known.status, "{host}: {reply:?}");
        assert_eq!(reply.body, known.body, "{host}: {reply:?}");
        for name in ["content-type", "cache-control", "content-security-policy"] {
            assert_eq!(reply.header(name), known.header(name), "{host} {name}");
        }
    }
    site.running.shutdown().await;
}
