// SPDX-License-Identifier: AGPL-3.0-or-later
//! A client of Puddle's loopback HTTP API and its server-sent event stream, for tests that run a
//! real `puddle serve`.
#![expect(
    clippy::indexing_slicing,
    reason = "a malformed reply or event stream fails the test at the line that reads it"
)]

use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// A client of the API, over plain HTTP/1.1 (the API listens on loopback).
pub struct Api {
    addr: String,
    token: String,
}

impl Api {
    /// A client of the API at `url` (`http://127.0.0.1:<port>/`) with bearer `token`.
    #[must_use]
    pub fn from_url(url: &str, token: &str) -> Self {
        Self {
            addr: url
                .strip_prefix("http://")
                .unwrap_or_else(|| panic!("not a loopback http URL: {url}"))
                .trim_end_matches('/')
                .to_owned(),
            token: token.to_owned(),
        }
    }

    /// One request; the status and the JSON body (`Null` when the body is not JSON).
    pub async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let mut stream = TcpStream::connect(&self.addr)
            .await
            .expect("loopback API I/O");
        let body = body.map(Value::to_string).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            self.token,
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("loopback API I/O");
        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .await
            .expect("loopback API I/O");
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("no status in {head:?}"));
        let rest = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            dechunk(rest)
        } else {
            rest.to_owned()
        };
        (status, serde_json::from_str(&rest).unwrap_or(Value::Null))
    }

    /// Workspace `id` as the API shows it; panics on a non-200 answer.
    pub async fn workspace(&self, id: &str) -> Value {
        let (status, body) = self
            .call("GET", &format!("/api/workspaces/{id}"), None)
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }

    /// Opens the event stream; events from here on are seen.
    pub async fn events(&self) -> Events {
        let mut stream = TcpStream::connect(&self.addr)
            .await
            .expect("loopback API I/O");
        let request = format!(
            "GET /api/events HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\
             Accept: text/event-stream\r\n\r\n",
            self.addr, self.token
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("loopback API I/O");
        let mut events = Events {
            stream,
            buf: String::new(),
        };
        events
            .fill(|b| b.contains("\r\n\r\n"), Duration::from_secs(10))
            .await;
        assert!(events.buf.starts_with("HTTP/1.1 200"), "{}", events.buf);
        events.buf = events
            .buf
            .split_once("\r\n\r\n")
            .expect("loopback API I/O")
            .1
            .to_owned();
        events
    }
}

/// The API's server-sent events, read as they arrive.
pub struct Events {
    stream: TcpStream,
    buf: String,
}

impl Events {
    /// Reads until `done` holds for what was read; panics at the deadline.
    async fn fill(&mut self, done: impl Fn(&str) -> bool, within: Duration) {
        let deadline = Instant::now() + within;
        let mut chunk = [0_u8; 4096];
        while !done(&self.buf) {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.stream.read(&mut chunk)).await {
                Ok(Ok(0)) => panic!("the event stream ended; buffer: {:?}", self.buf),
                Ok(Ok(n)) => self.buf.push_str(&String::from_utf8_lossy(&chunk[..n])),
                Ok(Err(e)) => panic!("event stream: {e}"),
                Err(elapsed) => panic!(
                    "nothing more within {within:?} ({elapsed}); buffer: {:?}",
                    self.buf
                ),
            }
        }
    }

    /// The `workspace_progress` event that ends workspace `id`'s operation (`done` or `failed`).
    // flake-ok: reads the server-sent event stream (it consumes input, so a re-run probe cannot
    // do it); bounded by `within` (an image pull takes minutes, past a probe's limit), and the
    // panic shows what was read.
    pub async fn ended(&mut self, id: &str, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            while let Some(end) = self.buf.find("\n\n") {
                let record: String = self.buf.drain(..end + 2).collect();
                let data: String = record
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect();
                if let Ok(e) = serde_json::from_str::<Value>(&data)
                    && e["type"] == "workspace_progress"
                    && e["workspace"] == id
                    && matches!(e["step"].as_str(), Some("done" | "failed"))
                {
                    return e;
                }
            }
            let before = self.buf.len();
            let left = deadline.saturating_duration_since(Instant::now());
            self.fill(|b| b.len() > before && b.contains("\n\n"), left)
                .await;
        }
    }
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if n == 0 {
            break;
        }
        out.push_str(after.get(..n).unwrap_or(after));
        rest = after.get(n + 2..).unwrap_or("");
    }
    out
}
