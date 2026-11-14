// SPDX-License-Identifier: AGPL-3.0-or-later
//! Browser sign-in over real HTTP under a public name: the first visitor, passwords, the cookie,
//! CSRF and origin checks, session fixation, brute force, devices and revoking, and that no answer
//! carries a credential it should not.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use puddle_api::{
    ApiConfig, ApiServer, ApiToken, EventHub, MemorySettings, NoNames, PublicHost, Remote,
    RemoteServing, RunningApi, Services, SettingsRepo, reset_account,
};
use puddle_store::{Clock as _, Limits, ManualClock, Store, UserName};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use crate::common::{Reply, START_MS, STEP, raw};

pub(super) const HOST: &str = "puddle.example.org";
pub(super) const ORIGIN: &str = "https://puddle.example.org";
/// A test password made at run time, the same one for the same label. A literal handed to the
/// hasher is flagged as a hard-coded cryptographic value, and none of these protects anything.
pub(super) fn pw(label: &str) -> &'static str {
    static SUFFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let suffix = SUFFIX.get_or_init(|| {
        let mut bytes = [0_u8; 6];
        getrandom::fill(&mut bytes).unwrap();
        bytes.iter().fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
    });
    Box::leak(format!("{label} {suffix}").into_boxed_str())
}

pub(super) fn password() -> &'static str {
    pw("correct horse battery")
}
pub(super) const COOKIE: &str = "__Host-puddle-session";

pub(super) struct Site {
    pub(super) running: RunningApi,
    pub(super) addr: SocketAddr,
    pub(super) install_token: String,
    pub(super) store: Arc<Store>,
    pub(super) clock: Arc<ManualClock>,
    /// The database file, which the server's command line opens by itself.
    path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

pub(super) async fn site_with(multi_user: bool) -> Site {
    site_built(multi_user, |_, _| None).await
}

/// A site whose workspaces are the ones `workspaces` makes from the event hub and the clock.
pub(super) async fn site_built(
    multi_user: bool,
    workspaces: impl FnOnce(
        &Arc<EventHub>,
        &Arc<ManualClock>,
    ) -> Option<Arc<dyn puddle_api::WorkspaceService>>,
) -> Site {
    let clock = Arc::new(ManualClock::new(START_MS));
    let events = Arc::new(EventHub::default());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = Arc::new(
        Store::open(&path, clock.clone(), Limits::default())
            .unwrap()
            .with_events(events.clone()),
    );
    let made = workspaces(&events, &clock);
    let mut services = Services::new(
        store.clone(),
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        events,
        clock.clone(),
    )
    .with_front_resolver(Arc::new(NoNames))
    .with_account_secrets(store.take_account_secrets().unwrap());
    if let Some(made) = made {
        services = services.with_workspaces(made);
    }
    let mut config = ApiConfig::default();
    config.remote = Some(Remote::new(
        RemoteServing::new(vec![PublicHost::parse(HOST).unwrap()], None).unwrap(),
    ));
    config.multi_user = multi_user;
    let token = ApiToken::generate().unwrap();
    let server = ApiServer::bind(config, token.clone(), services)
        .await
        .unwrap();
    let addr = server.local_addr();
    Site {
        running: server.spawn(),
        addr,
        install_token: token.expose().to_owned(),
        store,
        clock,
        path,
        _dir: dir,
    }
}

pub(super) async fn site() -> Site {
    site_with(false).await
}

/// How a request presents itself.
#[derive(Default, Clone)]
pub(super) struct Req<'a> {
    host: Option<&'a str>,
    origin: Option<&'a str>,
    fetch_site: Option<&'a str>,
    bearer: Option<&'a str>,
    cookie: Option<&'a str>,
}

impl<'a> Req<'a> {
    /// A page of the site itself.
    pub(super) fn page() -> Self {
        Self {
            origin: Some(ORIGIN),
            ..Self::default()
        }
    }

    pub(super) fn bearer(token: &'a str) -> Self {
        Self {
            bearer: Some(token),
            ..Self::default()
        }
    }

    pub(super) fn cookie(mut self, cookie: &'a str) -> Self {
        self.cookie = Some(cookie);
        self
    }

    fn origin(mut self, origin: &'a str) -> Self {
        self.origin = Some(origin);
        self
    }

    fn fetch_site(mut self, site: &'a str) -> Self {
        self.fetch_site = Some(site);
        self
    }

    pub(super) fn on(mut self, host: &'a str) -> Self {
        self.host = Some(host);
        self
    }
}

fn head(method: &str, path: &str, req: &Req<'_>, body: Option<&Value>) -> String {
    let mut text = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        req.host.unwrap_or(HOST)
    );
    if let Some(origin) = req.origin {
        let _ = write!(text, "Origin: {origin}\r\n");
    }
    if let Some(site) = req.fetch_site {
        let _ = write!(text, "Sec-Fetch-Site: {site}\r\n");
    }
    if let Some(token) = req.bearer {
        let _ = write!(text, "Authorization: Bearer {token}\r\n");
    }
    if let Some(cookie) = req.cookie {
        let _ = write!(text, "Cookie: {cookie}\r\n");
    }
    let body = body.map(Value::to_string).unwrap_or_default();
    if !body.is_empty() || matches!(method, "POST" | "PUT") {
        let _ = write!(
            text,
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        );
    }
    text.push_str("\r\n");
    text.push_str(&body);
    text
}

impl Site {
    pub(super) async fn send(
        &self,
        method: &str,
        path: &str,
        req: &Req<'_>,
        body: Option<&Value>,
    ) -> Reply {
        raw(self.addr, head(method, path, req, body).as_bytes()).await
    }

    pub(super) async fn setup(&self) -> Reply {
        self.send(
            "POST",
            "/api/auth/setup",
            &Req::page(),
            Some(&json!({"name": "Tijs", "password": password(), "device_name": "desktop"})),
        )
        .await
    }

    pub(super) async fn sign_in(&self, name: &str, password: &str) -> Reply {
        self.send(
            "POST",
            "/api/auth/sign-in",
            &Req::page(),
            Some(&json!({"name": name, "password": password})),
        )
        .await
    }

    /// `puddle user reset <name>`: another process on the same database file.
    fn reset(&self, name: &str) -> puddle_api::ResetInvite {
        let cli = Store::open(&self.path, self.clock.clone(), Limits::default()).unwrap();
        let secrets = cli.take_account_secrets().unwrap();
        reset_account(&cli, &secrets, name, self.clock.now_ms()).unwrap()
    }

    pub(super) fn loopback_host(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }
}

/// The refresh cookie's value in a reply, and the whole `Set-Cookie` line.
pub(super) fn set_cookie(reply: &Reply) -> (String, String) {
    let line = reply
        .header("set-cookie")
        .unwrap_or_else(|| panic!("no Set-Cookie in {reply:?}"))
        .to_owned();
    let value = line
        .strip_prefix(&format!("{COOKIE}="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (value, line)
}

pub(super) fn access_token(reply: &Reply) -> String {
    reply.json()["access_token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn the_status_is_public_and_names_no_one() {
    let site = site().await;
    let reply = site
        .send("GET", "/api/auth/status", &Req::default(), None)
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_eq!(
        reply.json(),
        json!({"setup_open": true, "password_login": true})
    );
    site.setup().await;
    let after = site
        .send("GET", "/api/auth/status", &Req::default(), None)
        .await;
    assert_eq!(
        after.json(),
        json!({"setup_open": false, "password_login": true})
    );
    site.running.shutdown().await;
}

#[tokio::test]
async fn the_sign_in_routes_do_not_exist_on_the_loopback_address() {
    let site = site().await;
    let host = site.loopback_host();
    let token = site.install_token.clone();
    for (method, path) in [
        ("GET", "/api/auth/status"),
        ("POST", "/api/auth/setup"),
        ("POST", "/api/auth/sign-in"),
        ("POST", "/api/auth/invite"),
        ("POST", "/api/auth/refresh"),
        ("POST", "/api/auth/sign-out"),
    ] {
        // The desktop window and the command line have the install token and never sign in.
        let with_token = site
            .send(
                method,
                path,
                &Req::bearer(&token).on(&host),
                (method == "POST").then(|| json!({})).as_ref(),
            )
            .await;
        assert_eq!(with_token.status, 404, "{method} {path}: {with_token:?}");
        // Without it they are as protected as any other route.
        let without = site
            .send(method, path, &Req::default().on(&host), None)
            .await;
        assert_eq!(without.status, 401, "{method} {path}: {without:?}");
    }
    site.running.shutdown().await;
}

#[tokio::test]
async fn the_first_visitor_makes_the_owner_and_gets_a_cookie_and_a_token() {
    let site = site().await;
    let reply = site.setup().await;
    assert_eq!(reply.status, 201, "{reply:?}");
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    let (refresh, line) = set_cookie(&reply);
    for part in [
        "Path=/",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Max-Age=2592000",
    ] {
        assert!(line.contains(part), "{line}");
    }
    assert!(
        !line.contains("Domain"),
        "a `__Host-` cookie has no Domain: {line}"
    );
    assert_eq!(refresh.len(), 43);
    let body = reply.json();
    assert_eq!(
        body["user"],
        json!({"id": 1, "name": "Tijs", "role": "owner"})
    );
    assert_eq!(body["device"]["name"], "desktop");
    assert_eq!(body["device"]["current"], true);
    // The long-lived part never travels in a body, and the short-lived one is not the cookie.
    assert!(!reply.body.contains(&refresh));
    let access = access_token(&reply);
    assert_ne!(access, refresh);
    // The access token is an ordinary bearer token under the public name.
    let me = site
        .send("GET", "/api/pending", &Req::bearer(&access), None)
        .await;
    assert_eq!(me.status, 200, "{me:?}");
    // And only under it: the loopback address takes the install token.
    let host = site.loopback_host();
    let on_loopback = site
        .send("GET", "/api/pending", &Req::bearer(&access).on(&host), None)
        .await;
    assert_eq!(on_loopback.status, 401, "{on_loopback:?}");
    // The install token is not a credential for a browser under the public name.
    let install = site
        .send(
            "GET",
            "/api/pending",
            &Req::bearer(&site.install_token),
            None,
        )
        .await;
    assert_eq!(install.status, 401);
    site.running.shutdown().await;
}

#[tokio::test]
async fn setup_works_once() {
    let site = site().await;
    assert_eq!(site.setup().await.status, 201);
    let second = site.setup().await;
    assert_eq!(second.status, 409, "{second:?}");
    assert_eq!(second.error(), "conflict");
    assert!(second.header("set-cookie").is_none());
    site.running.shutdown().await;
}

/// Every route that sets or reads the cookie is a `POST` that must come from the page itself.
#[tokio::test]
async fn the_cookie_routes_refuse_requests_that_are_not_from_the_sites_own_page() {
    let site = site().await;
    let granted = site.setup().await;
    let (refresh, _) = set_cookie(&granted);
    let cookie = format!("{COOKIE}={refresh}");
    let body = json!({"name": "Tijs", "password": password()});
    let bodies = [
        ("/api/auth/refresh", None),
        ("/api/auth/sign-out", None),
        ("/api/auth/sign-in", Some(&body)),
        ("/api/auth/setup", Some(&body)),
        ("/api/auth/invite", Some(&body)),
    ];
    for (path, body) in bodies {
        let attempts = [
            ("no origin at all", Req::default().cookie(&cookie)),
            (
                "a page of another site",
                Req::default()
                    .cookie(&cookie)
                    .origin("https://evil.example"),
            ),
            (
                "an origin that only looks alike",
                Req::default()
                    .cookie(&cookie)
                    .origin("https://puddle.example.org.evil.example"),
            ),
            (
                "the null origin",
                Req::default().cookie(&cookie).origin("null"),
            ),
            (
                "plain http",
                Req::default()
                    .cookie(&cookie)
                    .origin("http://puddle.example.org"),
            ),
            (
                "our origin but made cross-site",
                Req::page().cookie(&cookie).fetch_site("cross-site"),
            ),
            (
                "our origin but made same-site",
                Req::page().cookie(&cookie).fetch_site("same-site"),
            ),
        ];
        for (what, req) in attempts {
            let reply = site.send("POST", path, &req, body).await;
            assert_eq!(reply.status, 403, "{path} with {what}: {reply:?}");
            assert_eq!(reply.error(), "forbidden_origin", "{path} with {what}");
        }
    }
    // The refused sign-out did not sign anyone out.
    let again = site
        .send(
            "POST",
            "/api/auth/refresh",
            &Req::page().cookie(&cookie),
            None,
        )
        .await;
    assert_eq!(again.status, 200, "{again:?}");
    site.running.shutdown().await;
}

#[tokio::test]
async fn a_form_post_from_another_site_cannot_even_try() {
    let site = site().await;
    // What a cross-site HTML form can send: a simple content type, the browser's own origin.
    let text = "POST /api/auth/sign-in HTTP/1.1\r\nHost: puddle.example.org\r\nOrigin: https://evil.example\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
    let reply = raw(site.addr, text.as_bytes()).await;
    assert_eq!(reply.status, 403, "{reply:?}");
    site.running.shutdown().await;
}

#[tokio::test]
async fn a_refresh_needs_the_cookie_and_gives_a_new_access_token() {
    let site = site().await;
    let granted = site.setup().await;
    let (refresh, _) = set_cookie(&granted);
    let cookie = format!("{COOKIE}={refresh}");
    let reply = site
        .send(
            "POST",
            "/api/auth/refresh",
            &Req::page().cookie(&cookie),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_ne!(access_token(&reply), access_token(&granted));
    assert!(
        reply.header("set-cookie").is_none(),
        "the cookie is not reissued"
    );
    let fresh = site
        .send(
            "GET",
            "/api/pending",
            &Req::bearer(&access_token(&reply)),
            None,
        )
        .await;
    assert_eq!(fresh.status, 200);

    for (what, header) in [
        ("no cookie", None),
        ("another name", Some(format!("session={refresh}"))),
        ("a wrong value", Some(format!("{COOKIE}=nonsense"))),
        ("an empty value", Some(format!("{COOKIE}="))),
        (
            "two of them",
            Some(format!("{COOKIE}={refresh}; {COOKIE}={refresh}")),
        ),
        (
            "the access token",
            Some(format!("{COOKIE}={}", access_token(&granted))),
        ),
    ] {
        let req = match &header {
            Some(header) => Req::page().cookie(header),
            None => Req::page(),
        };
        let reply = site.send("POST", "/api/auth/refresh", &req, None).await;
        assert_eq!(reply.status, 401, "{what}: {reply:?}");
        assert!(
            reply
                .header("set-cookie")
                .is_some_and(|c| c.contains("Max-Age=0")),
            "{what}: a dead cookie is cleared"
        );
    }
    site.running.shutdown().await;
}

/// A session cannot be planted: nothing a client sends picks or reuses a session at sign-in.
#[tokio::test]
async fn signing_in_never_takes_over_a_session_the_client_names() {
    let site = site().await;
    site.setup().await;
    let planted = format!("{COOKIE}=attackerchosenvalueattackerchosenvalue1");
    let reply = site
        .send(
            "POST",
            "/api/auth/sign-in",
            &Req::page().cookie(&planted),
            Some(&json!({"name": "Tijs", "password": password()})),
        )
        .await;
    assert_eq!(reply.status, 200, "{reply:?}");
    let (issued, _) = set_cookie(&reply);
    assert_ne!(issued, "attackerchosenvalueattackerchosenvalue1");
    // The value the attacker knows is no session, before or after the victim signed in.
    let attacker = site
        .send(
            "POST",
            "/api/auth/refresh",
            &Req::page().cookie(&planted),
            None,
        )
        .await;
    assert_eq!(attacker.status, 401);
    // Signing in with a live session's cookie makes another device, it does not reuse that one.
    let first = site.sign_in("Tijs", password()).await;
    let (first_refresh, _) = set_cookie(&first);
    let live = format!("{COOKIE}={first_refresh}");
    let second = site
        .send(
            "POST",
            "/api/auth/sign-in",
            &Req::page().cookie(&live),
            Some(&json!({"name": "Tijs", "password": password()})),
        )
        .await;
    let (second_refresh, _) = set_cookie(&second);
    assert_ne!(first_refresh, second_refresh);
    assert_ne!(first.json()["device"]["id"], second.json()["device"]["id"]);
    // A body that names a session is refused outright.
    let smuggled = site
        .send(
            "POST",
            "/api/auth/sign-in",
            &Req::page(),
            Some(&json!({"name": "Tijs", "password": password(), "session": "x", "refresh_token": "y"})),
        )
        .await;
    assert_eq!(smuggled.status, 422, "{smuggled:?}");
    site.running.shutdown().await;
}

#[tokio::test]
async fn a_wrong_password_is_refused_the_same_as_a_wrong_name_and_echoes_nothing() {
    let site = site().await;
    site.setup().await;
    let wrong_password = site.sign_in("Tijs", pw("not the password!")).await;
    let wrong_name = site.sign_in("nobody", pw("not the password!")).await;
    assert_eq!(wrong_password.status, 401);
    assert_eq!(wrong_name.status, 401);
    assert_eq!(wrong_password.body, wrong_name.body);
    assert!(!wrong_password.body.contains("not the password"));
    assert!(wrong_password.header("set-cookie").is_none());
    // A password that is not a string is refused without quoting it.
    let number = site
        .send(
            "POST",
            "/api/auth/sign-in",
            &Req::page(),
            Some(&json!({"name": "Tijs", "password": 123_456_789_012_345_u64})),
        )
        .await;
    assert_eq!(number.status, 422, "{number:?}");
    assert!(!number.body.contains("123456789012345"));
    site.running.shutdown().await;
}

#[tokio::test]
async fn brute_force_is_stopped_by_a_lock_that_the_right_password_does_not_lift() {
    let site = site().await;
    site.setup().await;
    for _ in 0..5 {
        let reply = site.sign_in("Tijs", pw("not the password!")).await;
        assert_eq!(reply.status, 401);
    }
    for password in [pw("not the password!"), password()] {
        let locked = site.sign_in("Tijs", password).await;
        assert_eq!(locked.status, 429, "{locked:?}");
        assert_eq!(locked.error(), "rate_limited");
        let wait: u64 = locked.header("retry-after").unwrap().parse().unwrap();
        assert!((1..=30).contains(&wait), "{wait}");
        assert!(locked.header("set-cookie").is_none());
    }
    // Another name is not affected, and a spoofed forwarding header buys nothing.
    let mut forged = head(
        "POST",
        "/api/auth/sign-in",
        &Req::page(),
        Some(&json!({"name": "Tijs", "password": pw("not the password!")})),
    );
    forged = forged.replace(
        "Connection: close\r\n",
        "Connection: close\r\nX-Forwarded-For: 203.0.113.9\r\nForwarded: for=203.0.113.9\r\n",
    );
    let reply = raw(site.addr, forged.as_bytes()).await;
    assert_eq!(reply.status, 429, "{reply:?}");
    site.clock.advance(30_000);
    let back = site.sign_in("Tijs", password()).await;
    assert_eq!(back.status, 200, "{back:?}");
    site.running.shutdown().await;
}

#[tokio::test]
async fn devices_are_listed_and_revoked_and_a_revoked_one_is_out_everywhere() {
    let site = site().await;
    let one = site.setup().await;
    let two = site.sign_in("Tijs", password()).await;
    let (access_one, access_two) = (access_token(&one), access_token(&two));
    let (refresh_one, _) = set_cookie(&one);
    let list = site
        .send("GET", "/api/devices", &Req::bearer(&access_two), None)
        .await;
    assert_eq!(list.status, 200, "{list:?}");
    let devices = list.json()["devices"].as_array().unwrap().clone();
    assert_eq!(devices.len(), 2);
    let current: Vec<&Value> = devices.iter().filter(|d| d["current"] == true).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0]["id"], two.json()["device"]["id"]);
    for device in &devices {
        let mut keys: Vec<&str> = device
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "created_at",
                "current",
                "expires_at",
                "id",
                "last_seen_at",
                "name"
            ]
        );
    }
    // Device two signs device one out.
    let id_one = one.json()["device"]["id"].as_i64().unwrap();
    let revoke = site
        .send(
            "DELETE",
            &format!("/api/devices/{id_one}"),
            &Req::bearer(&access_two),
            None,
        )
        .await;
    assert_eq!(revoke.status, 204, "{revoke:?}");
    let dead = site
        .send("GET", "/api/pending", &Req::bearer(&access_one), None)
        .await;
    assert_eq!(dead.status, 401, "{dead:?}");
    let refresh = site
        .send(
            "POST",
            "/api/auth/refresh",
            &Req::page().cookie(&format!("{COOKIE}={refresh_one}")),
            None,
        )
        .await;
    assert_eq!(refresh.status, 401);
    // Device two carries on, and a device that is gone answers as one that never was.
    let alive = site
        .send("GET", "/api/pending", &Req::bearer(&access_two), None)
        .await;
    assert_eq!(alive.status, 200);
    for id in [id_one.to_string(), "9999".to_owned()] {
        let missing = site
            .send(
                "DELETE",
                &format!("/api/devices/{id}"),
                &Req::bearer(&access_two),
                None,
            )
            .await;
        assert_eq!(missing.status, 404, "{id}: {missing:?}");
    }
    let not_a_number = site
        .send(
            "DELETE",
            "/api/devices/abc",
            &Req::bearer(&access_two),
            None,
        )
        .await;
    assert_eq!(not_a_number.status, 400);
    site.running.shutdown().await;
}

/// An open event stream of a device ends when that device is revoked.
#[tokio::test]
async fn revoking_a_device_ends_its_open_event_stream() {
    let site = site().await;
    let one = site.setup().await;
    let two = site.sign_in("Tijs", password()).await;
    let request = head(
        "GET",
        "/api/events",
        &Req::bearer(&access_token(&one)),
        None,
    )
    .replace("Connection: close\r\n", "Accept: text/event-stream\r\n");
    let mut tcp = TcpStream::connect(site.addr).await.unwrap();
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut seen = String::new();
    let mut chunk = [0u8; 2048];
    tokio::time::timeout(STEP, async {
        while !seen.contains("\r\n\r\n") {
            let n = tcp.read(&mut chunk).await.unwrap();
            assert!(n > 0, "closed early: {seen}");
            seen.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    })
    .await
    .unwrap();
    assert!(seen.starts_with("HTTP/1.1 200"), "{seen}");
    let id = one.json()["device"]["id"].as_i64().unwrap();
    let revoke = site
        .send(
            "DELETE",
            &format!("/api/devices/{id}"),
            &Req::bearer(&access_token(&two)),
            None,
        )
        .await;
    assert_eq!(revoke.status, 204);
    // The body ends with the last chunk (the connection itself is kept alive).
    let closed = tokio::time::timeout(STEP, async {
        while !seen.ends_with("0\r\n\r\n") {
            let n = tcp.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "the stream is still open after the revoke: {seen}"
    );
    site.running.shutdown().await;
}

#[tokio::test]
async fn signing_out_clears_the_cookie_and_revokes_the_device() {
    let site = site().await;
    let granted = site.setup().await;
    let (refresh, _) = set_cookie(&granted);
    let cookie = format!("{COOKIE}={refresh}");
    let out = site
        .send(
            "POST",
            "/api/auth/sign-out",
            &Req::page().cookie(&cookie),
            None,
        )
        .await;
    assert_eq!(out.status, 204, "{out:?}");
    assert!(out.header("set-cookie").unwrap().contains("Max-Age=0"));
    let after = site
        .send(
            "GET",
            "/api/pending",
            &Req::bearer(&access_token(&granted)),
            None,
        )
        .await;
    assert_eq!(after.status, 401);
    // Signing out when not signed in is fine.
    let again = site
        .send("POST", "/api/auth/sign-out", &Req::page(), None)
        .await;
    assert_eq!(again.status, 204);
    site.running.shutdown().await;
}

#[tokio::test]
async fn the_password_changes_with_the_current_one_and_not_from_the_desktop() {
    let site = site().await;
    let granted = site.setup().await;
    let other = site.sign_in("Tijs", password()).await;
    let access = access_token(&granted);
    let body = json!({"current": password(), "new": pw("a brand new password")});
    let host = site.loopback_host();
    let desktop = site
        .send(
            "PUT",
            "/api/auth/password",
            &Req::bearer(&site.install_token).on(&host),
            Some(&body),
        )
        .await;
    assert_eq!(desktop.status, 403, "{desktop:?}");
    let wrong = site
        .send(
            "PUT",
            "/api/auth/password",
            &Req::bearer(&access),
            Some(&json!({"current": pw("not the password!"), "new": pw("a brand new password")})),
        )
        .await;
    assert_eq!(wrong.status, 401, "{wrong:?}");
    let changed = site
        .send(
            "PUT",
            "/api/auth/password",
            &Req::bearer(&access),
            Some(&body),
        )
        .await;
    assert_eq!(changed.status, 204, "{changed:?}");
    let kept = site
        .send("GET", "/api/devices", &Req::bearer(&access), None)
        .await;
    assert_eq!(kept.status, 200);
    let gone = site
        .send(
            "GET",
            "/api/devices",
            &Req::bearer(&access_token(&other)),
            None,
        )
        .await;
    assert_eq!(gone.status, 401);
    assert_eq!(site.sign_in("Tijs", password()).await.status, 401);
    assert_eq!(
        site.sign_in("Tijs", pw("a brand new password"))
            .await
            .status,
        200
    );
    site.running.shutdown().await;
}

#[tokio::test]
async fn an_invite_from_the_server_sets_a_password_and_works_once() {
    let site = site().await;
    let granted = site.setup().await;
    let access = access_token(&granted);
    let invite = site.reset("tijs");

    // Everything signed in is out, though the server was never told.
    let out = site
        .send("GET", "/api/pending", &Req::bearer(&access), None)
        .await;
    assert_eq!(out.status, 401, "{out:?}");
    assert_eq!(site.sign_in("Tijs", password()).await.status, 401);
    let status = site
        .send("GET", "/api/auth/status", &Req::default(), None)
        .await;
    assert_eq!(
        status.json()["setup_open"],
        false,
        "a reset never reopens setup"
    );

    let body = json!({"secret": invite.secret.as_str(), "password": pw("brand new long password")});
    let no_origin = site
        .send("POST", "/api/auth/invite", &Req::default(), Some(&body))
        .await;
    assert_eq!(no_origin.status, 403);
    let used = site
        .send("POST", "/api/auth/invite", &Req::page(), Some(&body))
        .await;
    assert_eq!(used.status, 200, "{used:?}");
    assert!(used.header("set-cookie").is_some());
    let twice = site
        .send("POST", "/api/auth/invite", &Req::page(), Some(&body))
        .await;
    assert_eq!(twice.status, 401, "{twice:?}");
    assert_eq!(
        site.sign_in("Tijs", pw("brand new long password"))
            .await
            .status,
        200
    );
    site.running.shutdown().await;
}

#[tokio::test]
async fn the_owner_changes_the_sign_in_methods_and_the_last_one_stays_on() {
    let site = site().await;
    let granted = site.setup().await;
    let access = access_token(&granted);
    let read = site
        .send("GET", "/api/auth/methods", &Req::bearer(&access), None)
        .await;
    assert_eq!(
        read.json(),
        json!({"methods": [{"method": "password", "enabled": true}]})
    );
    let off = site
        .send(
            "PUT",
            "/api/auth/methods",
            &Req::bearer(&access),
            Some(&json!({"method": "password", "enabled": false})),
        )
        .await;
    assert_eq!(off.status, 422, "{off:?}");
    let missing = site
        .send(
            "PUT",
            "/api/auth/methods",
            &Req::bearer(&access),
            Some(&json!({"method": "passkey", "enabled": true})),
        )
        .await;
    assert_eq!(missing.status, 422);

    // With a second method, passwords can go off and nobody signs in with one.
    site.store
        .register_sign_in_method(puddle_store::SignInMethod::Passkey)
        .unwrap();
    let off = site
        .send(
            "PUT",
            "/api/auth/methods",
            &Req::bearer(&access),
            Some(&json!({"method": "password", "enabled": false})),
        )
        .await;
    assert_eq!(off.status, 200, "{off:?}");
    assert_eq!(
        site.send("GET", "/api/auth/status", &Req::default(), None)
            .await
            .json()["password_login"],
        false
    );
    let refused = site.sign_in("Tijs", password()).await;
    assert_eq!(refused.status, 403, "{refused:?}");
    site.running.shutdown().await;
}

#[tokio::test]
async fn a_plain_user_signs_in_when_several_users_are_on_and_is_not_the_owner() {
    let site = site_with(true).await;
    let owner = site.setup().await;
    site.store.add_user(&UserName::new("ann").unwrap()).unwrap();
    let invite = site.reset("ann");
    let joined = site
        .send(
            "POST",
            "/api/auth/invite",
            &Req::page(),
            Some(&json!({"secret": invite.secret.as_str(), "password": pw("ann's long password"), "device_name": "ann's phone"})),
        )
        .await;
    assert_eq!(joined.status, 200, "{joined:?}");
    assert_eq!(joined.json()["user"]["role"], "user");
    let ann = access_token(&joined);

    // Her devices are hers; the owner's are not hers to see or to revoke.
    let mine = site
        .send("GET", "/api/devices", &Req::bearer(&ann), None)
        .await;
    let devices = mine.json()["devices"].as_array().unwrap().clone();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["name"], "ann's phone");
    let owners_id = owner.json()["device"]["id"].as_i64().unwrap();
    let revoke = site
        .send(
            "DELETE",
            &format!("/api/devices/{owners_id}"),
            &Req::bearer(&ann),
            None,
        )
        .await;
    assert_eq!(revoke.status, 404, "{revoke:?}");
    assert_eq!(
        site.send(
            "GET",
            "/api/devices",
            &Req::bearer(&access_token(&owner)),
            None
        )
        .await
        .status,
        200
    );
    // And none of the install's own settings.
    for (method, path, body) in [
        ("GET", "/api/auth/methods", None),
        (
            "PUT",
            "/api/auth/methods",
            Some(json!({"method": "password", "enabled": true})),
        ),
        ("GET", "/api/settings", None),
    ] {
        let reply = site
            .send(method, path, &Req::bearer(&ann), body.as_ref())
            .await;
        assert_eq!(reply.status, 403, "{method} {path}: {reply:?}");
        assert_eq!(reply.error(), "forbidden");
    }
    site.running.shutdown().await;
}

/// No answer to a signed-in browser carries a password, a hash, a refresh token or an invite:
/// not the lists, not the errors, not a body echoing what was sent.
#[tokio::test]
async fn nothing_a_signed_in_browser_can_read_carries_a_credential() {
    let site = site().await;
    let granted = site.setup().await;
    let (refresh, _) = set_cookie(&granted);
    let access = access_token(&granted);
    let second = site.sign_in("Tijs", password()).await;
    let (refresh_two, _) = set_cookie(&second);
    let secrets = [
        password(),
        "argon2",
        refresh.as_str(),
        refresh_two.as_str(),
        "$argon2id",
    ];
    let mut answers = Vec::new();
    for path in [
        "/api/devices",
        "/api/auth/status",
        "/api/auth/methods",
        "/api/health",
        "/api/pending",
        "/api/settings",
        "/api/audit",
        "/api/pending",
        "/api/rules",
        "/api/openapi.json",
    ] {
        answers.push((
            path,
            site.send("GET", path, &Req::bearer(&access), None).await,
        ));
    }
    answers.push((
        "refresh",
        site.send(
            "POST",
            "/api/auth/refresh",
            &Req::page().cookie(&format!("{COOKIE}={refresh}")),
            None,
        )
        .await,
    ));
    answers.push((
        "weak password",
        site.send(
            "PUT",
            "/api/auth/password",
            &Req::bearer(&access),
            Some(&json!({"current": password(), "new": "short"})),
        )
        .await,
    ));
    answers.push((
        "wrong password",
        site.send(
            "PUT",
            "/api/auth/password",
            &Req::bearer(&access),
            Some(&json!({"current": pw("wrong password here"), "new": "short"})),
        )
        .await,
    ));
    for (path, reply) in &answers {
        let text = format!("{reply:?}");
        for secret in secrets {
            assert!(!text.contains(secret), "{path} carries a secret");
        }
        if *path != "refresh" {
            assert!(reply.header("set-cookie").is_none(), "{path} sets a cookie");
        }
    }
    // The access token itself appears in no answer but a sign-in's.
    for (path, reply) in &answers {
        if *path != "refresh" {
            assert!(
                !reply.body.contains(&access),
                "{path} repeats the access token"
            );
        }
    }
    site.running.shutdown().await;
}

#[tokio::test]
async fn a_request_naming_a_session_in_a_url_or_a_header_is_not_a_session() {
    let site = site().await;
    let granted = site.setup().await;
    let (refresh, _) = set_cookie(&granted);
    let access = access_token(&granted);
    // Neither the cookie value nor a query parameter stands in for the bearer token.
    let cookie_only = format!("{COOKIE}={refresh}");
    for req in [
        Req::default().cookie(&cookie_only),
        Req::bearer(&refresh),
        Req::default(),
    ] {
        let reply = site.send("GET", "/api/devices", &req, None).await;
        assert_eq!(reply.status, 401, "{reply:?}");
    }
    let in_url = site
        .send(
            "GET",
            &format!("/api/devices?access_token={access}"),
            &Req::default(),
            None,
        )
        .await;
    assert_eq!(in_url.status, 401);
    site.running.shutdown().await;
}

#[tokio::test]
async fn with_remote_serving_off_there_is_nobody_to_sign_in_to() {
    let events = Arc::new(EventHub::default());
    let clock = Arc::new(ManualClock::new(START_MS));
    let store = Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap());
    let services = Services::new(
        store.clone(),
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        events,
        clock,
    )
    .with_account_secrets(store.take_account_secrets().unwrap());
    let token = ApiToken::generate().unwrap();
    let server = ApiServer::bind(ApiConfig::default(), token.clone(), services)
        .await
        .unwrap();
    let addr = server.local_addr();
    let running = server.spawn();
    let host = format!("127.0.0.1:{}", addr.port());
    let reply = raw(
        addr,
        head(
            "GET",
            "/api/auth/status",
            &Req::bearer(token.expose()).on(&host),
            None,
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(reply.status, 404, "{reply:?}");
    let public = raw(
        addr,
        head("GET", "/api/auth/status", &Req::default(), None).as_bytes(),
    )
    .await;
    assert_eq!(public.status, 421, "a public name is refused: {public:?}");
    running.shutdown().await;
}

#[tokio::test]
async fn without_accounts_the_routes_answer_unavailable() {
    let clock = Arc::new(ManualClock::new(START_MS));
    let store = Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap());
    let services = Services::new(
        store,
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        Arc::new(EventHub::default()),
        clock,
    )
    .with_front_resolver(Arc::new(NoNames));
    let mut config = ApiConfig::default();
    config.remote = Some(Remote::new(
        RemoteServing::new(vec![PublicHost::parse(HOST).unwrap()], None).unwrap(),
    ));
    let server = ApiServer::bind(config, ApiToken::generate().unwrap(), services)
        .await
        .unwrap();
    let addr = server.local_addr();
    let running = server.spawn();
    let reply = raw(
        addr,
        head("GET", "/api/auth/status", &Req::default(), None).as_bytes(),
    )
    .await;
    assert_eq!(reply.status, 503, "{reply:?}");
    // And no bearer token is ever accepted under the public name.
    let token = raw(
        addr,
        head("GET", "/api/health", &Req::bearer("anything"), None).as_bytes(),
    )
    .await;
    assert_eq!(token.status, 401);
    running.shutdown().await;
}

/// A terminal's WebSocket made for a signed-in browser closes when that browser is signed out.
#[tokio::test]
async fn signing_a_device_out_closes_its_open_terminal() {
    let mut config = ApiConfig::default();
    config.remote = Some(Remote::new(
        RemoteServing::new(vec![PublicHost::parse(HOST).unwrap()], None).unwrap(),
    ));
    let api = crate::common::start_with(config).await;
    let made = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "box", "repositories": [{"url": "https://github.com/acme/api.git"}]})),
        )
        .await;
    assert_eq!(made.status, 202, "{made:?}");
    api.workspaces.idle().await;
    let send = |method: &'static str, path: String, req: Req<'static>, body: Option<Value>| {
        let addr = api.addr;
        async move { raw(addr, head(method, &path, &req, body.as_ref()).as_bytes()).await }
    };
    let credentials = json!({"name": "Tijs", "password": password()});
    let one = send(
        "POST",
        "/api/auth/setup".into(),
        Req::page(),
        Some(credentials.clone()),
    )
    .await;
    assert_eq!(one.status, 201, "{one:?}");
    let two = send(
        "POST",
        "/api/auth/sign-in".into(),
        Req::page(),
        Some(credentials),
    )
    .await;
    let (access_one, access_two) = (access_token(&one), access_token(&two));
    let access_one: &'static str = Box::leak(access_one.into_boxed_str());
    let access_two: &'static str = Box::leak(access_two.into_boxed_str());
    let made = send(
        "POST",
        "/api/workspaces/box/terminals".into(),
        Req::bearer(access_one),
        Some(json!({"cols": 80, "rows": 24})),
    )
    .await;
    assert_eq!(made.status, 201, "{made:?}");
    let (terminal, ticket) = (
        made.json()["terminal"].as_u64().unwrap(),
        made.json()["ticket"].as_str().unwrap().to_owned(),
    );
    let mut socket = crate::common::RawSocket::open(
        api.addr,
        &format!("/api/workspaces/box/terminals/{terminal}"),
        Some(HOST),
        &format!("{}, ticket.{ticket}", puddle_api::SUBPROTOCOL),
    )
    .await;
    let id = one.json()["device"]["id"].as_i64().unwrap();
    let revoked = send(
        "DELETE",
        format!("/api/devices/{id}"),
        Req::bearer(access_two),
        None,
    )
    .await;
    assert_eq!(revoked.status, 204, "{revoked:?}");
    assert_eq!(socket.read_close().await, 1008);
    // The host waits for the page's close before it drops the connection.
    socket.answer_close_after_a_pause().await;
    api.running.shutdown().await;
}
