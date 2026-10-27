// SPDX-License-Identifier: GPL-3.0-or-later
//! The front through real sockets: a client on the loopback speaks HTTP to the front, which carries
//! it to a fake editor server over a second socket. What a request must look like to be routed,
//! what the server sees of it, what the client sees of the answer, and how `WebSockets` and the
//! secret key behave.
#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_front::{
    Editor, FileHalves, Flavour, Front, Guests, Io, ServerHalves, Target, Unreachable,
};
use puddle_netpolicy::{EndpointKind, PuddleEndpoints};
use puddle_types::WorkspaceName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the fake editor server was asked.
#[derive(Default)]
struct Seen {
    heads: Mutex<Vec<String>>,
    connections: AtomicUsize,
}

/// A fake editor server: answers `/whoami` with its name, `/head` with the head it received,
/// `/csp` with a policy of its own, `/ws` with an upgrade and then an echo.
struct Server {
    addr: SocketAddr,
    seen: Arc<Seen>,
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    String::from_utf8(head).ok()
}

impl Server {
    async fn start(name: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Seen::default());
        let task_seen = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                task_seen.connections.fetch_add(1, Ordering::SeqCst);
                let seen = Arc::clone(&task_seen);
                tokio::spawn(async move {
                    let Some(head) = read_head(&mut stream).await else {
                        return;
                    };
                    let target = head.split_whitespace().nth(1).unwrap_or("");
                    let path = target.split('?').next().unwrap_or("").to_owned();
                    seen.heads.lock().unwrap().push(head.clone());
                    let reply = |extra: &str, body: &str| {
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n{body}",
                            body.len()
                        )
                    };
                    match path.as_str() {
                        "/ws" => {
                            stream
                                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nconnection: Upgrade\r\nupgrade: websocket\r\nsec-websocket-accept: x\r\n\r\n")
                                .await
                                .unwrap();
                            let mut buf = [0_u8; 256];
                            while let Ok(n) = stream.read(&mut buf).await {
                                if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        }
                        "/head" => {
                            let _ = stream.write_all(reply("", &head).as_bytes()).await;
                        }
                        "/csp" => {
                            let extra = "content-security-policy: default-src 'self'\r\nset-cookie: server=1\r\n";
                            let _ = stream.write_all(reply(extra, "ok").as_bytes()).await;
                        }
                        _ => {
                            let _ = stream.write_all(reply("", name).as_bytes()).await;
                        }
                    }
                });
            }
        });
        Self { addr, seen }
    }
}

/// An editor reached over a plain socket, or one that cannot be reached.
struct Reach(Option<SocketAddr>);

impl Editor for Reach {
    fn connect(&self) -> puddle_front::editor::BoxFuture<'_, Result<Io, Unreachable>> {
        Box::pin(async move {
            let addr = self
                .0
                .ok_or_else(|| Unreachable("nothing listens in the workspace".into()))?;
            Ok(Box::new(TcpStream::connect(addr).await.unwrap()) as Io)
        })
    }

    fn password(&self) -> puddle_front::editor::BoxFuture<'_, Result<String, Unreachable>> {
        Box::pin(async { Ok("sekrit".to_owned()) })
    }
}

#[derive(Default)]
struct Running(Mutex<HashMap<String, Target>>);

impl Running {
    fn start(&self, name: &str, addr: Option<SocketAddr>, flavour: Flavour) {
        self.0.lock().unwrap().insert(
            name.to_owned(),
            Target {
                editor: Arc::new(Reach(addr)),
                flavour,
            },
        );
    }
}

impl Guests for Running {
    fn target(&self, workspace: &WorkspaceName) -> Option<Target> {
        self.0.lock().unwrap().get(workspace.as_str()).cloned()
    }
}

struct Rig {
    front: Front,
    running: Arc<Running>,
    endpoints: PuddleEndpoints,
    dir: tempfile::TempDir,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let running = Arc::new(Running::default());
    let endpoints = PuddleEndpoints::new();
    let halves: Arc<dyn ServerHalves> = Arc::new(FileHalves::new(dir.path().join("front")));
    let front = Front::bind(None, &endpoints, running.clone(), halves)
        .await
        .unwrap();
    Rig {
        front,
        running,
        endpoints,
        dir,
    }
}

struct Answer {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Vec<&str> {
        self.head
            .lines()
            .skip(1)
            .filter_map(|l| l.split_once(": "))
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
            .collect()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Sends `request` (a full request text) to the front and reads the answer to its end.
async fn ask(front: &Front, request: &str) -> Answer {
    let mut stream = TcpStream::connect(front.local_addr()).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("the front did not finish its answer")
        .unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("no head in the answer");
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    Answer {
        status,
        head,
        body: raw[split + 4..].to_vec(),
    }
}

fn get(rig: &Rig, host: &str, path: &str, extra: &str) -> String {
    let _ = rig;
    format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{extra}\r\n")
}

fn host(rig: &Rig, name: &str) -> String {
    format!("{name}.localhost:{}", rig.front.local_addr().port())
}

fn ws(name: &WorkspaceName) -> WorkspaceName {
    name.clone()
}

#[tokio::test]
async fn each_workspace_name_reaches_its_own_editor_and_only_its_own() {
    let rig = rig().await;
    let (a, b) = (Server::start("alpha").await, Server::start("beta").await);
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    rig.running.start("b", Some(b.addr), Flavour::CodeServer);
    for (name, expected) in [("a", "alpha"), ("b", "beta"), ("a", "alpha")] {
        let h = host(&rig, name);
        let answer = ask(&rig.front, &get(&rig, &h, "/whoami", "")).await;
        assert_eq!((answer.status, answer.text()), (200, expected.to_owned()));
    }
    assert_eq!(a.seen.connections.load(Ordering::SeqCst), 2);
    assert_eq!(b.seen.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_connect_request_is_refused_and_reaches_nothing() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let h = host(&rig, "a");
    let request = format!("CONNECT {h} HTTP/1.1\r\nHost: {h}\r\nConnection: close\r\n\r\n");
    let answer = ask(&rig.front, &request).await;
    assert_eq!(answer.status, 405);
    assert_eq!(a.seen.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_name_that_is_not_a_running_workspace_gets_a_404_and_reaches_nothing() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let h = host(&rig, "stopped");
    let answer = ask(&rig.front, &get(&rig, &h, "/", "")).await;
    assert_eq!(answer.status, 404);
    assert!(answer.text().contains("stopped"), "{}", answer.text());
    assert_eq!(a.seen.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_foreign_host_is_refused_and_the_editor_never_hears_of_it() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let port = rig.front.local_addr().port();
    for host in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("a.example.test:{port}"),
        format!("a.localhost.example.test:{port}"),
        format!("x.a.localhost:{port}"),
        format!("a.localhost:{}", port.wrapping_add(1)),
        "a.localhost".to_owned(),
        format!("tauri.localhost:{port}"),
        format!("ipc.localhost:{port}"),
        format!("asset.localhost:{port}"),
    ] {
        let answer = ask(&rig.front, &get(&rig, &host, "/", "")).await;
        assert_eq!(answer.status, 421, "{host}: {}", answer.head);
    }
    // No Host at all (HTTP/1.0), and two of them.
    let none = ask(&rig.front, "GET / HTTP/1.0\r\n\r\n").await;
    assert_eq!(none.status, 400);
    let h = host(&rig, "a");
    let two = ask(
        &rig.front,
        &format!("GET / HTTP/1.1\r\nHost: {h}\r\nHost: {h}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(two.status, 400);
    // An absolute-form target that names another authority than the Host.
    let mixed = ask(
        &rig.front,
        &format!("GET http://evil.test/ HTTP/1.1\r\nHost: {h}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(mixed.status, 421);
    assert_eq!(a.seen.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn another_origin_is_refused_and_the_editors_own_origin_is_not() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let h = host(&rig, "a");
    for origin in [
        "http://b.localhost:1".to_owned(),
        "http://evil.test".to_owned(),
        "null".to_owned(),
        format!("https://{h}"),
    ] {
        let answer = ask(
            &rig.front,
            &get(&rig, &h, "/", &format!("Origin: {origin}\r\n")),
        )
        .await;
        assert_eq!(answer.status, 403, "{origin}");
    }
    assert_eq!(a.seen.connections.load(Ordering::SeqCst), 0);
    let own = ask(
        &rig.front,
        &get(&rig, &h, "/whoami", &format!("Origin: http://{h}\r\n")),
    )
    .await;
    assert_eq!(own.status, 200);
}

#[tokio::test]
async fn the_editor_gets_the_request_without_hop_by_hop_headers_and_with_the_host_it_was_asked_for()
{
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let h = host(&rig, "a");
    let answer = ask(
        &rig.front,
        &get(
            &rig,
            &h,
            "/head?x=1",
            "Keep-Alive: 5\r\nTE: trailers\r\nProxy-Authorization: x\r\nCookie: a=b\r\n",
        ),
    )
    .await;
    let seen = answer.text().to_ascii_lowercase();
    assert!(seen.starts_with("get /head?x=1 http/1.1"), "{seen}");
    assert!(seen.contains(&format!("host: {h}")), "{seen}");
    assert!(seen.contains("cookie: a=b"), "{seen}");
    for gone in ["keep-alive", "te:", "proxy-authorization"] {
        assert!(!seen.contains(gone), "{gone} passed on: {seen}");
    }
}

#[tokio::test]
async fn every_answer_carries_the_frame_headers_next_to_the_servers_own_policy() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let h = host(&rig, "a");
    let answer = ask(&rig.front, &get(&rig, &h, "/csp", "")).await;
    assert_eq!(
        answer.header("content-security-policy"),
        vec!["default-src 'self'", "frame-ancestors 'self'"]
    );
    assert_eq!(answer.header("x-frame-options"), vec!["SAMEORIGIN"]);
    // code-server gets no cookie of the front's, and keeps its own.
    assert_eq!(answer.header("set-cookie"), vec!["server=1"]);
}

#[tokio::test]
async fn an_editor_that_cannot_be_reached_is_a_502_that_says_why_and_cannot_run_script() {
    let rig = rig().await;
    rig.running.start("a", None, Flavour::CodeServer);
    let h = host(&rig, "a");
    let answer = ask(&rig.front, &get(&rig, &h, "/", "")).await;
    assert_eq!(answer.status, 502);
    assert!(answer.text().contains("nothing listens in the workspace"));
    assert_eq!(answer.header("x-content-type-options"), vec!["nosniff"]);
    assert_eq!(
        answer.header("content-security-policy"),
        vec!["default-src 'none'; sandbox"]
    );
}

/// Opens a WebSocket-style upgrade and returns the stream after the 101.
async fn upgrade(rig: &Rig, host: &str, extra: &str) -> (TcpStream, String) {
    let mut stream = TcpStream::connect(rig.front.local_addr()).await.unwrap();
    let request = format!(
        "GET /ws HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: abc\r\nSec-WebSocket-Version: 13\r\n{extra}\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let head = read_head(&mut stream).await.unwrap();
    (stream, head)
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..500 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn a_websocket_upgrade_carries_bytes_both_ways_and_is_counted_while_it_is_open() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let name = WorkspaceName::new("a").unwrap();
    let h = host(&rig, "a");
    assert_eq!(rig.front.connected(&name), 0);
    let (mut one, head) = upgrade(&rig, &h, &format!("Origin: http://{h}\r\n")).await;
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    let (mut two, _) = upgrade(&rig, &h, "").await;
    until("two counted", || rig.front.connected(&ws(&name)) == 2).await;
    for (stream, word) in [(&mut one, &b"first"[..]), (&mut two, b"second")] {
        stream.write_all(word).await.unwrap();
        let mut back = vec![0_u8; word.len()];
        stream.read_exact(&mut back).await.unwrap();
        assert_eq!(back, word);
    }
    drop(one);
    until("one left", || rig.front.connected(&ws(&name)) == 1).await;
    drop(two);
    until("none left", || rig.front.connected(&ws(&name)) == 0).await;
}

#[tokio::test]
async fn a_websocket_from_another_origin_is_refused_before_the_editor_hears_of_it() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let h = host(&rig, "a");
    let (_stream, head) = upgrade(&rig, &h, "Origin: http://b.localhost:1\r\n").await;
    assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    assert_eq!(a.seen.connections.load(Ordering::SeqCst), 0);
    assert_eq!(rig.front.connected(&WorkspaceName::new("a").unwrap()), 0);
}

#[tokio::test]
async fn closing_the_front_ends_its_websockets_and_frees_its_port() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let h = host(&rig, "a");
    let (mut stream, _) = upgrade(&rig, &h, "").await;
    let addr = rig.front.local_addr();
    rig.front.close().await;
    let mut buf = [0_u8; 8];
    let end = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
        .await
        .expect("the socket outlived the front");
    assert!(matches!(end, Ok(0) | Err(_)), "{end:?}");
    assert!(
        TcpListener::bind(addr).await.is_ok(),
        "the port is still held"
    );
    drop(rig.dir);
}

const MINT: &str = "/_puddle/mint-key";

fn cookie_value(answer: &Answer, name: &str) -> Option<String> {
    answer.header("set-cookie").into_iter().find_map(|c| {
        c.split(';')
            .next()?
            .strip_prefix(&format!("{name}="))
            .map(str::to_owned)
    })
}

async fn mint(rig: &Rig, host: &str, cookie: Option<&str>) -> Answer {
    let cookie = cookie.map_or(String::new(), |c| format!("Cookie: {c}\r\n"));
    ask(
        &rig.front,
        &format!("POST {MINT} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: 0\r\n{cookie}\r\n"),
    )
    .await
}

#[tokio::test]
async fn microsofts_server_gets_the_secret_key_cookie_and_a_key_that_holds_across_requests() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::Microsoft);
    let h = host(&rig, "a");

    // A page load sets both cookies, and the server never sees the client half.
    let page = ask(&rig.front, &get(&rig, &h, "/head", "")).await;
    assert_eq!(
        cookie_value(&page, "vscode-secret-key-path").as_deref(),
        Some(MINT)
    );
    let half = cookie_value(&page, "puddle-secret-half").expect("no client half cookie");
    assert!(
        page.header("set-cookie")
            .iter()
            .any(|c| c.contains("HttpOnly"))
    );
    let with_cookie = ask(
        &rig.front,
        &get(
            &rig,
            &h,
            "/head",
            &format!("Cookie: puddle-secret-half={half}; a=b\r\n"),
        ),
    )
    .await;
    let seen = with_cookie.text();
    assert!(seen.contains("cookie: a=b"), "{seen}");
    assert!(
        !seen.contains("puddle-secret-half"),
        "the half reached the server: {seen}"
    );

    // The mint answers the same 32 bytes for the same client half, other bytes for another half.
    let cookie = format!("puddle-secret-half={half}");
    let first = mint(&rig, &h, Some(&cookie)).await;
    let second = mint(&rig, &h, Some(&cookie)).await;
    assert_eq!((first.status, first.body.len()), (200, 32));
    assert_eq!(first.body, second.body);
    assert_eq!(
        cookie_value(&first, "puddle-secret-half"),
        Some(half.clone())
    );
    let stranger = mint(&rig, &h, None).await;
    assert_ne!(stranger.body, first.body);
    assert!(cookie_value(&stranger, "puddle-secret-half").is_some());
    // The mint never reaches the server.
    assert_eq!(a.seen.connections.load(Ordering::SeqCst), 2);
    assert_eq!(first.header("x-frame-options"), vec!["SAMEORIGIN"]);

    // Not by GET.
    let by_get = ask(&rig.front, &get(&rig, &h, MINT, "")).await;
    assert_eq!(by_get.status, 405);
}

#[tokio::test]
async fn the_key_survives_a_new_front_and_differs_between_workspaces() {
    let first = rig().await;
    let a = Server::start("alpha").await;
    first.running.start("a", Some(a.addr), Flavour::Microsoft);
    first.running.start("b", Some(a.addr), Flavour::Microsoft);
    let client = "puddle-secret-half=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let key_a = mint(&first, &host(&first, "a"), Some(client)).await.body;
    let key_b = mint(&first, &host(&first, "b"), Some(client)).await.body;
    assert_ne!(key_a, key_b);

    // A new front over the same folder (Puddle restarted).
    let running = Arc::new(Running::default());
    running.start("a", Some(a.addr), Flavour::Microsoft);
    let halves: Arc<dyn ServerHalves> = Arc::new(FileHalves::new(first.dir.path().join("front")));
    let front = Front::bind(None, &PuddleEndpoints::new(), running, halves)
        .await
        .unwrap();
    let h = format!("a.localhost:{}", front.local_addr().port());
    let again = ask(
        &front,
        &format!("POST {MINT} HTTP/1.1\r\nHost: {h}\r\nConnection: close\r\nContent-Length: 0\r\nCookie: {client}\r\n\r\n"),
    )
    .await;
    assert_eq!(again.body, key_a);

    // Forgetting the workspace gives the name a new key.
    front.forget(&WorkspaceName::new("a").unwrap()).unwrap();
    let after = ask(
        &front,
        &format!("POST {MINT} HTTP/1.1\r\nHost: {h}\r\nConnection: close\r\nContent-Length: 0\r\nCookie: {client}\r\n\r\n"),
    )
    .await;
    assert_ne!(after.body, key_a);
}

#[tokio::test]
async fn code_servers_mint_path_is_the_servers_own_and_sets_no_cookie() {
    let rig = rig().await;
    let a = Server::start("alpha").await;
    rig.running.start("a", Some(a.addr), Flavour::CodeServer);
    let h = host(&rig, "a");
    let answer = mint(&rig, &h, None).await;
    assert_eq!(answer.text(), "alpha");
    assert!(cookie_value(&answer, "puddle-secret-half").is_none());
}

#[tokio::test]
async fn the_front_is_one_of_puddles_own_listeners_on_the_loopback_only() {
    let rig = rig().await;
    let addr = rig.front.local_addr();
    assert!(addr.ip().is_loopback());
    assert_eq!(
        rig.endpoints.list(),
        vec![(addr, EndpointKind::BrowserFront)]
    );
    rig.front.close().await;
}

#[tokio::test]
async fn the_usual_port_is_kept_when_free_and_replaced_when_taken() {
    let free = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let make = |preferred| async move {
        let dir = tempfile::tempdir().unwrap();
        let halves: Arc<dyn ServerHalves> = Arc::new(FileHalves::new(dir.path()));
        let front = Front::bind(
            Some(preferred),
            &PuddleEndpoints::new(),
            Arc::new(Running::default()),
            halves,
        )
        .await
        .unwrap();
        (front, dir)
    };
    let (kept, _d1) = make(free).await;
    assert_eq!(kept.local_addr().port(), free);
    assert!(!kept.preferred_port_was_taken());
    let (moved, _d2) = make(free).await;
    assert_ne!(moved.local_addr().port(), free);
    assert!(moved.preferred_port_was_taken());
    assert_eq!(
        moved.origin(&WorkspaceName::new("a").unwrap()),
        format!("http://a.localhost:{}", moved.local_addr().port())
    );
}

#[tokio::test]
async fn the_password_comes_from_the_workspaces_editor_and_only_while_it_runs() {
    let rig = rig().await;
    rig.running.start("a", None, Flavour::CodeServer);
    let a = WorkspaceName::new("a").unwrap();
    assert_eq!(rig.front.password(&a).await.unwrap(), "sekrit");
    let err = rig
        .front
        .password(&WorkspaceName::new("b").unwrap())
        .await
        .unwrap_err();
    assert!(err.0.contains("not running"), "{err}");
}

#[tokio::test]
async fn the_front_and_its_targets_print_without_leaking_what_they_hold() {
    let rig = rig().await;
    rig.running.start("a", None, Flavour::CodeServer);
    let shown = format!("{:?}", rig.front);
    assert!(
        shown.contains("Guests") && shown.contains("ServerHalves"),
        "{shown}"
    );
    let target = format!("{:?}", rig.running.0.lock().unwrap().get("a").unwrap());
    assert!(
        target.contains("CodeServer") && !target.contains("sekrit"),
        "{target}"
    );
    let a = WorkspaceName::new("a").unwrap();
    assert_eq!(rig.front.windows().connected(&a), 0);
}
