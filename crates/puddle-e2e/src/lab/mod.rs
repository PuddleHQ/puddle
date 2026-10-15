// SPDX-License-Identifier: GPL-3.0-or-later
//! Corporate-network fixtures: the things a client network puts between a machine and the
//! internet, started as real programs on free loopback ports for one test each.
//!
//! | Fixture | Stands in for | Needs |
//! |---|---|---|
//! | [`Squid`] | an explicit proxy, open or with Basic authentication, with split DNS | `PUDDLE_SQUID_BIN` or `PUDDLE_SQUID_DOCKER` (Unix) |
//! | [`Interceptor`] | a TLS-intercepting proxy with a root made for the run | `PUDDLE_MITMDUMP_BIN` |
//! | [`NtlmProxy`] | a proxy that demands NTLM and accepts any Type 3 | `PUDDLE_MITMDUMP_BIN` |
//! | [`PacServer`] | the web server that hands out a PAC file | nothing |
//!
//! A fixture a test cannot start because its program is missing makes `start` return `None` and
//! print why; when the matching `PUDDLE_REQUIRE_*` variable is set (CI sets it) a missing program
//! is a failure instead. Every fixture has a per-run [`Prefix`] in its directory, process and
//! container names, binds `127.0.0.1` on a port the OS chose, holds no real credential, and stops
//! when dropped.
//!
//! Test and development code only.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::missing_panics_doc,
    reason = "fixtures fail the test that started them by panicking"
)]

mod interceptor;
mod pac;
#[cfg(unix)]
mod squid;

use std::net::{Ipv4Addr, TcpStream};
use std::path::Path;
use std::process::Child;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub use interceptor::{Interceptor, NtlmLeg, NtlmProxy, Root};
pub use pac::{PacServer, failover_pac};
#[cfg(unix)]
pub use squid::{Squid, SquidOptions};

/// The loopback address every fixture binds.
pub const LOCAL: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// The Basic user of the lab's authenticating proxy.
pub const BASIC_USER: &str = "alice";
/// The Basic password: a lab constant, protecting nothing.
pub const BASIC_PASSWORD: &str = "lab-only-pw-7Qx2";

/// How long a fixture may take to accept connections before its start fails the test.
const READY: Duration = Duration::from_secs(60);

/// A name unique to one fixture in one run (process id and a counter), used in directory,
/// process and container names so concurrent runs and tests never collide.
#[derive(Clone, Debug)]
pub struct Prefix(String);

impl Prefix {
    /// A fresh prefix for `kind` (`squid`, `mitm`, ...).
    #[must_use]
    pub fn new(kind: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(format!("puddle-lab-{kind}-{}-{n}", std::process::id()))
    }

    /// The prefix text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A free loopback port. The port can be taken again before the caller binds it; the fixtures
/// retry nothing, so a collision is a failed start, not a silent one.
#[must_use]
pub fn free_port() -> u16 {
    std::net::TcpListener::bind((LOCAL, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Waits until `port` accepts connections, or panics with `diagnostics()`.
fn wait_ready(port: u16, child: Option<&mut Child>, diagnostics: impl Fn() -> String) {
    let deadline = Instant::now() + READY;
    let mut child = child;
    while Instant::now() < deadline {
        if TcpStream::connect((LOCAL, port)).is_ok() {
            return;
        }
        if let Some(Ok(Some(status))) = child.as_deref_mut().map(Child::try_wait) {
            panic!("the fixture exited early ({status})\n{}", diagnostics());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the fixture did not accept connections\n{}", diagnostics());
}

/// A file's text, or empty when it is missing.
fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Kills `child` and waits for it.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
