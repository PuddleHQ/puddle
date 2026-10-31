// SPDX-License-Identifier: GPL-3.0-or-later
//! The web server that hands out a PAC file, and a builder for the failover lists tests need.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;

use super::LOCAL;

/// Serves one PAC body on `http://127.0.0.1:<port>/<unique name>.pac`, from a thread, so it works
/// inside or outside a Tokio runtime. Stopped on drop.
///
/// The name is new for every server: WinHTTP keeps every script it fetched under its URL, for the
/// whole machine and after the server is gone, so a server that gets a port another test's server
/// once had would be answered from that test's script.
pub struct PacServer {
    port: u16,
    path: String,
    hits: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PacServer {
    /// Starts the server for `body`.
    ///
    /// # Panics
    /// When no loopback port can be bound.
    #[must_use]
    pub fn start(body: &str) -> Self {
        let listener = TcpListener::bind((LOCAL, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let stopping = Arc::new(AtomicBool::new(false));
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let thread = std::thread::spawn({
            let (hits, stopping) = (hits.clone(), stopping.clone());
            move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let mut buf = [0_u8; 2048];
                    let _ = stream.read(&mut buf); // the request line and headers are not needed
                    hits.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(response.as_bytes());
                }
            }
        });
        Self {
            port,
            path: format!("/{}.pac", puddle_types::fixture::unique("proxy")),
            hits,
            stopping,
            thread: Some(thread),
        }
    }

    /// The PAC file's URL.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{LOCAL}:{}{}", self.port, self.path)
    }

    /// How many times the file was fetched.
    #[must_use]
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for PacServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect((LOCAL, self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A PAC script: names ending in one of `direct_suffixes` go `DIRECT`, everything else tries
/// `hops` (`host:port` each) in order and then goes `DIRECT`, as `PROXY a; PROXY b; DIRECT`.
#[must_use]
pub fn failover_pac(direct_suffixes: &[&str], hops: &[&str]) -> String {
    let mut script = String::from("function FindProxyForURL(url, host) {\n");
    for suffix in direct_suffixes {
        let _ = writeln!(
            script,
            "  if (dnsDomainIs(host, \"{suffix}\")) return \"DIRECT\";"
        );
    }
    script.push_str("  return \"");
    for hop in hops {
        let _ = write!(script, "PROXY {hop}; ");
    }
    script.push_str("DIRECT\";\n}\n");
    script
}
