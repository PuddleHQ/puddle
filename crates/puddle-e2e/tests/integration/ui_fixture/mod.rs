// SPDX-License-Identifier: AGPL-3.0-or-later
//! The UI fixture backend over real HTTP: seeded data, the guard of the real API, the
//! control server, scripted events, restart and reset. The shared harness is here; each folder
//! module is one area, and `real_routes` is the group that runs product code.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test: a failed step fails it by panicking"
)]

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use puddle_e2e::ui_fixture::{
    Fixture, FixtureOptions, Scenario, Step, built_in_names, control, load_scenario,
    step_from_drop, watch_drop_dir,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const STEP: Duration = Duration::from_secs(10);

struct Running {
    fixture: Arc<Fixture>,
    api: std::net::SocketAddr,
    control: std::net::SocketAddr,
    token: String,
}

async fn start_scenario(scenario: Scenario) -> Running {
    let fixture = Fixture::start(FixtureOptions {
        port: 0,
        connection_file: None,
        scenario,
    })
    .await
    .unwrap();
    let (control, _task) = control::serve(fixture.clone(), 0).await.unwrap();
    let info = fixture.connection_info().await.unwrap();
    Running {
        api: fixture.addr().await.unwrap(),
        control,
        token: info.token.expose().to_owned(),
        fixture,
    }
}

async fn start(name: &str) -> Running {
    start_scenario(load_scenario(name).unwrap()).await
}

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("{e}: reply of {} bytes is not JSON", self.body.len()))
    }
}

async fn http(
    addr: std::net::SocketAddr,
    host: &str,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Reply {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(token) = token {
        let _ = write!(head, "Authorization: Bearer {token}\r\n");
    }
    let body = body.map(Value::to_string).unwrap_or_default();
    if method != "GET" {
        let _ = write!(
            head,
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        );
    }
    head.push_str("\r\n");
    head.push_str(&body);
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    tcp.write_all(head.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(STEP, tcp.read_to_end(&mut bytes))
        .await
        .expect("reply in time")
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    Reply {
        status,
        body: body.to_owned(),
    }
}

impl Running {
    async fn api(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("127.0.0.1:{}", self.api.port());
        http(self.api, &host, Some(&self.token), method, path, body).await
    }

    async fn get(&self, path: &str) -> Reply {
        self.api("GET", path, None).await
    }

    async fn control(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("127.0.0.1:{}", self.control.port());
        http(self.control, &host, Some(&self.token), method, path, body).await
    }

    async fn state(&self) -> Value {
        self.control("GET", "/control/state", None).await.json()
    }

    /// An open SSE stream on the API, returned once the response head has come: the handler has
    /// subscribed by then, so an event sent after this returns is on the stream. The head is read
    /// a byte at a time so that no event is consumed with it.
    async fn events(&self) -> TcpStream {
        let mut tcp = TcpStream::connect(self.api).await.unwrap();
        let request = format!(
            "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nAccept: text/event-stream\r\n\r\n",
            self.api.port(),
            self.token
        );
        tcp.write_all(request.as_bytes()).await.unwrap();
        let mut head = Vec::new();
        tokio::time::timeout(STEP, async {
            while !head.ends_with(b"\r\n\r\n") {
                head.push(tcp.read_u8().await.unwrap());
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no response head; saw {:?}", String::from_utf8_lossy(&head)));
        tcp
    }
}

/// Reads from `tcp` until `done` is true of what came, or fails after the step timeout.
async fn read_until(tcp: &mut TcpStream, done: impl Fn(&str) -> bool) -> String {
    let mut seen = String::new();
    let mut chunk = [0u8; 4096];
    tokio::time::timeout(STEP, async {
        while !done(&seen) {
            let n = tcp.read(&mut chunk).await.unwrap();
            assert!(n > 0, "stream closed early: {seen}");
            seen.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out; saw {seen:?}"));
    seen
}

mod control_server;
mod identities;
mod network;
mod real_routes;
mod request_rules;
mod scenarios;
mod settings_and_checks;
mod workspaces;
