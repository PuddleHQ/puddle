// SPDX-License-Identifier: AGPL-3.0-or-later
//! Corporate-network fixtures: the things a client network puts between a machine and the
//! internet, started as real programs on free loopback ports for one test each.
//!
//! | Fixture | Stands in for | Needs |
//! |---|---|---|
//! | [`Squid`] | an explicit proxy, open or with Basic authentication, with split DNS | `PUDDLE_SQUID_BIN` or `PUDDLE_SQUID_DOCKER` (Unix) |
//! | [`Interceptor`] | a TLS-intercepting proxy with a root made for the run | `PUDDLE_MITMDUMP_BIN` |
//! | [`NtlmProxy`] | a proxy that demands NTLM and accepts any Type 3 | `PUDDLE_MITMDUMP_BIN` |
//! | [`Forgejo`] | a real Git forge with HTTPS and its own SSH server, one account and a token made for the run | `PUDDLE_FORGEJO_BIN` (Forgejo on Unix; Gitea's Windows release on Windows) |
//! | [`PacServer`] | the web server that hands out a PAC file | nothing |
//! | [`WebHost`] | a public website the test only needs to reach, over plain HTTP and TLS | nothing |
//! | [`PrivateAccess`] | a private-access client's adapter, DNS answers and tunnel to an intranet site | nothing |
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

mod feeds;
mod forgejo;
mod git_host;
mod interceptor;
mod pac;
mod pip_index;
mod private_access;
mod procs;
#[cfg(unix)]
mod squid;
mod web_host;

use std::fs::{File, TryLockError};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub use feeds::{MAVEN_COORDINATES, NPM_PACKAGE, NUGET_PACKAGE, PIP_PROJECT, private_feeds};
pub use forgejo::{Forgejo, OWNER as FORGEJO_OWNER};
pub use git_host::{GitHost, GitHostOptions, NameProxy, Seen, Versions, clean_git, git_available};
pub use interceptor::{Interceptor, NtlmLeg, NtlmProxy, Root};
pub use pac::{PacServer, failover_pac};
pub use pip_index::pip_index;
pub use private_access::{Adapter, PrivateAccess};
#[cfg(unix)]
pub use squid::{Squid, SquidOptions};
pub use web_host::{Responder, WebAnswer, WebHost, WebOptions};

/// The loopback address every fixture binds.
pub const LOCAL: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// The Basic user of the lab's authenticating proxy.
pub const BASIC_USER: &str = "alice";
/// The Basic password of the lab proxy: random per test process, so the fixture holds no constant
/// credential. The proxy's helper script and the clients in the same process use the same value.
#[must_use]
pub fn basic_password() -> String {
    puddle_types::fixture::password("lab")
}

/// How long a fixture may take to accept connections before its start fails the test.
const READY: Duration = Duration::from_secs(60);

/// How many mitmproxy starts may run at once, across every test process of the machine.
///
/// A mitigation, not a measured cure: on the hosted Windows runner (4 vCPUs, nextest running 4
/// tests at once) a queue run lost two tests to the 60 s bound when four starts overlapped at a
/// moment the whole machine was slow (30 s to over 60 s each, against 1.5 s idle). The stall was
/// not reproduced; a start is mostly file reads through the antivirus and CPU, which overlapping
/// starts share, so fewer overlapping starts leave a stall fewer starts to multiply. A running
/// proxy costs nothing, so only the start waits, and only until the proxy listens.
const START_SLOTS: usize = 2;

/// A hold on one of the [`START_SLOTS`] start slots: an advisory lock on a file in the temp
/// folder, which the operating system releases when the holder ends, a crash included.
#[derive(Debug)]
struct StartSlot {
    // Held for its lock: closing the file releases it.
    _file: File,
}

impl StartSlot {
    /// The folder of the slot files, shared by every test process of the user.
    #[expect(
        clippy::disallowed_methods,
        reason = "the slots exist to be shared: a per-process temp folder would give every test process its own"
    )]
    fn shared_dir() -> PathBuf {
        std::env::temp_dir().join("puddle-lab-start-slots")
    }

    /// A free slot of `slots` in `dir` (made if missing), or `None` when all are held.
    fn try_take(dir: &Path, slots: usize) -> std::io::Result<Option<Self>> {
        std::fs::create_dir_all(dir)?;
        for n in 0..slots {
            let file = File::options()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.join(format!("slot-{n}")))?;
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { _file: file })),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(e)) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Waits for a free slot of `slots` in `dir`, or returns `None` when the slot files cannot
    /// be used (a folder another user made in a shared temp folder): the slots limit load and
    /// are not required. A holder is a start that ends in at most [`READY`] (it listens or
    /// panics), so the wait is bounded by the starts ahead and never by a stuck lock.
    fn take_from(dir: &Path, slots: usize, what: &str) -> Option<Self> {
        let begun = Instant::now();
        let mut waiting = false;
        let slot = puddle_test_kit::eventually_blocking(
            &format!("{what} gets a start slot"),
            puddle_test_kit::MAX_WAIT,
            || match Self::try_take(dir, slots) {
                Ok(Some(slot)) => Some(Some(slot)),
                Ok(None) => {
                    if !waiting {
                        eprintln!("lab: {what} waits for a start slot");
                        waiting = true;
                    }
                    None
                }
                Err(e) => {
                    eprintln!("lab: {what} starts without a start slot ({e})");
                    Some(None)
                }
            },
        );
        if slot.is_some() && waiting {
            eprintln!("lab: {what} got a start slot after {:.1?}", begun.elapsed());
        }
        slot
    }

    /// [`Self::take_from`] the shared folder with [`START_SLOTS`] slots.
    fn take(what: &str) -> Option<Self> {
        Self::take_from(&Self::shared_dir(), START_SLOTS, what)
    }
}

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

/// What a fixture's start cost: the wall-clock time from the call to the first accepted
/// connection, and the CPU time its processes had used by then.
///
/// The wall time is what a test waits and depends on how many other processes want the CPU
/// (a check runs niced on a host meant to run at 100 %, so the same start takes 1 s or 13 s).
/// The CPU time is the work the fixture itself does and does not stretch with the load, so it
/// is the part a bound on "how long a fixture takes to start" can hold the fixture to.
#[derive(Clone, Copy, Debug, Default)]
pub struct StartCost {
    /// From the call to the first accepted connection.
    pub wall: Duration,
    /// The user and system CPU time of the fixture's process and the processes it started, at
    /// that point; `None` where the OS does not say or the fixture runs in a container, where the
    /// wall time is the only measure and a loaded host can stretch it.
    pub cpu: Option<Duration>,
}

impl StartCost {
    /// Measures `child` (the fixture's process, if it is one of ours) and logs one line, so a
    /// slow start shows which of the two it was, and which process it was in.
    fn measure(prefix: &Prefix, begun: Instant, child: Option<&Child>) -> Self {
        let cost = Self {
            wall: begun.elapsed(),
            cpu: child.and_then(|c| procs::tree_cpu(c.id())),
        };
        eprintln!(
            "lab: {} ready after {:.2?} wall, {} CPU{}",
            prefix.as_str(),
            cost.wall,
            cost.cpu
                .map_or_else(|| "unknown".to_owned(), |c| format!("{c:.2?}")),
            child.map_or_else(String::new, |c| format!(" ({})", procs::tree_summary(c))),
        );
        cost
    }

    /// The time a start bound applies to: the CPU time where it is known, else the wall time.
    #[must_use]
    pub fn bounded(&self) -> Duration {
        self.cpu.unwrap_or(self.wall)
    }
}

/// A free loopback port. Nothing owns it once this returns, so a program that takes its port
/// by number (Squid reads it from its config file) may find it taken; start such a program
/// through `on_free_port`. A program that can bind port 0 and report it (mitmproxy) should do
/// that and never use this.
// flake-ok: Squid reads its port from a config file and inherits no socket, so the port cannot
// be held across its start; `on_free_port` retries on a taken port and a test of it forces one.
#[must_use]
pub fn free_port() -> u16 {
    std::net::TcpListener::bind((LOCAL, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// How many ports [`on_free_port`] tries before it gives up.
const PORT_ATTEMPTS: u32 = 5;

/// Whether a program's output says its bind failed because the port was taken (`EADDRINUSE`):
/// mitmproxy `[Errno 98] ... address already in use`, Squid `(98) Address already in use`,
/// Windows `WSAEADDRINUSE` / `only one usage of each socket address`.
fn is_address_in_use(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    [
        "address already in use",
        "wsaeaddrinuse",
        "only one usage of each socket address",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Starts a program that must be told its port in advance. `pick` gives a candidate port;
/// `start` runs the program on it and returns its output when it fails. A failure whose output
/// says the port was taken (another process bound it between the pick and the program's own
/// bind: nothing can hold a port across an `exec` for a program that takes no inherited socket)
/// is retried on a fresh port, up to [`PORT_ATTEMPTS`] times; any other failure panics at once.
///
/// # Panics
/// On a failure that is not a taken port, or when every attempt finds the port taken.
fn on_free_port<T>(
    mut pick: impl FnMut() -> u16,
    mut start: impl FnMut(u16) -> Result<T, String>,
) -> T {
    let mut last = String::new();
    for attempt in 1..=PORT_ATTEMPTS {
        let port = pick();
        match start(port) {
            Ok(started) => return started,
            Err(output) if is_address_in_use(&output) => {
                eprintln!("lab: port {port} was taken (attempt {attempt}), trying another");
                last = output;
            }
            Err(output) => panic!("the fixture failed to start on port {port}\n{output}"),
        }
    }
    panic!("the fixture found its port taken {PORT_ATTEMPTS} times\n{last}");
}

/// Whether a fixture whose program is not configured must fail its test (`required`, from its
/// `PUDDLE_REQUIRE_*` variable) instead of skipping.
///
/// # Panics
/// When `required` and the program is not `configured`.
fn configured_or_skip(configured: bool, required: bool, variable: &str, how: &str) -> bool {
    assert!(
        configured || !required,
        "{variable} is set but the fixture's program is not configured: {how}"
    );
    if !configured {
        // skip-ok: the assert above already failed when the caller's PUDDLE_REQUIRE_* flag is set
        eprintln!("skipped: {how}");
    }
    configured
}

/// Checks that a configured program path (`variable`) is a file.
///
/// # Panics
/// When it is not.
fn require_file(bin: &std::ffi::OsStr, variable: &str) {
    assert!(
        Path::new(bin).is_file(),
        "{variable}={} is not a file: install the program or fix the path",
        bin.to_string_lossy()
    );
}

/// Waits until `port` accepts connections. `Err` carries `diagnostics()` when `child` exits
/// first or nothing accepts within `ready`. A child that exited counts even when something
/// accepts on the port: that is another process (a collision), not the fixture.
fn try_ready(
    port: u16,
    child: Option<&mut Child>,
    ready: Duration,
    diagnostics: impl Fn() -> String,
) -> Result<(), String> {
    try_answers(
        child,
        ready,
        &diagnostics,
        || puddle_test_kit::accepts(SocketAddr::from((LOCAL, port))),
        // A child that dies right after something else answered is a collision too.
        Duration::from_millis(100),
        || false,
    )
}

/// Like [`try_ready`], with the caller's own `answers`: whether the program (and not a stranger on
/// its port) is serving. A program that exits first is a failure, even when `answers` says yes,
/// and so is one that `failed` says is alive but broken (a program that logs its failed bind and
/// stays up).
///
/// For a program that binds its port late, an accepting port proves nothing: another process can
/// hold the port the program failed to bind, and let go of it again, so a connect succeeds and the
/// next one is refused. `answers` must be something only the program can do.
fn try_answers(
    mut child: Option<&mut Child>,
    ready: Duration,
    diagnostics: &dyn Fn() -> String,
    mut answers: impl FnMut() -> bool,
    settle: Duration,
    mut failed: impl FnMut() -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + ready;
    let exited_early = |child: &mut Option<&mut Child>| {
        child
            .as_deref_mut()
            .map(Child::try_wait)
            .and_then(Result::ok)
            .flatten()
            .map(|status| format!("the fixture exited early ({status})\n{}", diagnostics()))
    };
    // flake-ok: a timeout must come back as `Err` for the caller (squid retries on it); the kit's waits panic.
    while Instant::now() < deadline {
        if let Some(why) = exited_early(&mut child) {
            return Err(why);
        }
        if failed() {
            return Err(format!("the fixture failed to start\n{}", diagnostics()));
        }
        if answers() {
            std::thread::sleep(settle);
            return exited_early(&mut child).map_or(Ok(()), Err);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "the fixture did not accept connections\n{}",
        diagnostics()
    ))
}

/// [`try_ready`] that fails the test.
///
/// # Panics
/// When `try_ready` returns `Err`.
fn wait_ready(
    port: u16,
    child: Option<&mut Child>,
    ready: Duration,
    diagnostics: impl Fn() -> String,
) {
    if let Err(why) = try_ready(port, child, ready, diagnostics) {
        panic!("{why}");
    }
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

#[cfg(test)]
mod tests;
