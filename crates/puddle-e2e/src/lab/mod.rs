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
//! | [`WebHost`] | a public website the test only needs to reach, over plain HTTP and TLS | nothing |
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

mod git_host;
mod interceptor;
mod pac;
#[cfg(unix)]
mod squid;
mod web_host;

use std::fs::{File, TryLockError};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub use git_host::{GitHost, GitHostOptions, NameProxy, Seen, Versions, clean_git, git_available};
pub use interceptor::{Interceptor, NtlmLeg, NtlmProxy, Root};
pub use pac::{PacServer, failover_pac};
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
/// connection, and the CPU time its process had used by then.
///
/// The wall time is what a test waits and depends on how many other processes want the CPU
/// (a check runs niced on a host meant to run at 100 %, so the same start takes 1 s or 13 s).
/// The CPU time is the work the fixture itself does and does not stretch with the load, so it
/// is the part a bound on "how long a fixture takes to start" can hold the fixture to.
#[derive(Clone, Copy, Debug, Default)]
pub struct StartCost {
    /// From the call to the first accepted connection.
    pub wall: Duration,
    /// The fixture process's user and system CPU time at that point; `None` where the OS does
    /// not say (no `/proc`) or the fixture runs in a container, where the wall time is the
    /// only measure and a loaded host can stretch it.
    pub cpu: Option<Duration>,
}

impl StartCost {
    /// Measures `child` (the fixture's process, if it is one of ours) and logs one line, so a
    /// slow start shows which of the two it was.
    fn measure(prefix: &Prefix, begun: Instant, child: Option<&Child>) -> Self {
        let cost = Self {
            wall: begun.elapsed(),
            cpu: child.and_then(|c| cpu_time(c.id())),
        };
        eprintln!(
            "lab: {} ready after {:.2?} wall, {} CPU",
            prefix.as_str(),
            cost.wall,
            cost.cpu
                .map_or_else(|| "unknown".to_owned(), |c| format!("{c:.2?}")),
        );
        cost
    }

    /// The time a start bound applies to: the CPU time where it is known, else the wall time.
    #[must_use]
    pub fn bounded(&self) -> Duration {
        self.cpu.unwrap_or(self.wall)
    }
}

/// User plus system CPU time of process `pid` so far, from Linux's `/proc/<pid>/stat`; `None`
/// where there is no such file.
fn cpu_time(pid: u32) -> Option<Duration> {
    parse_cpu_time(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// `utime + stime` of a `/proc/<pid>/stat` line, at the 100 Hz tick every Linux target uses. The
/// process name (field 2) is in parentheses and may hold spaces and parentheses, so the fields
/// are counted from the last `)`.
fn parse_cpu_time(stat: &str) -> Option<Duration> {
    let mut fields = stat[stat.rfind(')')? + 1..].split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(Duration::from_millis((utime + stime) * 10))
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

#[cfg(unix)]
/// How many ports [`on_free_port`] tries before it gives up.
const PORT_ATTEMPTS: u32 = 5;

#[cfg(unix)]
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

#[cfg(unix)]
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
    let deadline = Instant::now() + ready;
    let mut child = child;
    // flake-ok: a timeout must come back as `Err` for the caller (squid retries on it); the kit's waits panic.
    while Instant::now() < deadline {
        let exited = child.as_deref_mut().map(Child::try_wait);
        if let Some(Ok(Some(status))) = exited {
            return Err(format!(
                "the fixture exited early ({status})\n{}",
                diagnostics()
            ));
        }
        if puddle_test_kit::accepts(SocketAddr::from((LOCAL, port))) {
            // A child that dies right after something else answered is a collision too.
            std::thread::sleep(Duration::from_millis(100));
            if let Some(Ok(Some(status))) = child.as_deref_mut().map(Child::try_wait) {
                return Err(format!(
                    "the fixture exited early ({status})\n{}",
                    diagnostics()
                ));
            }
            return Ok(());
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
mod tests {
    use super::*;

    /// Tests that bind port 0, hand out a port they dropped, or spawn a child run one at a time. A
    /// test that binds port 0 can be handed the port another test just closed, and a child
    /// forked while a start-slot lock is held keeps that lock until it execs.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    use std::process::Command;

    #[test]
    fn only_as_many_starts_hold_a_slot_as_there_are_slots_and_a_dropped_one_frees_its_place() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let take = || StartSlot::try_take(dir.path(), 2).unwrap();
        let first = take().unwrap();
        let second = take().unwrap();
        assert!(take().is_none(), "a third start got a slot");
        drop(first);
        let third = take().expect("the freed slot was not offered");
        drop((second, third));
    }

    #[test]
    fn a_start_with_no_free_slot_waits_for_one_and_then_gets_it() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let held = StartSlot::try_take(dir.path(), 1).unwrap().unwrap();
        let release = std::thread::spawn(move || {
            // An ordering with no signal: the waiter polls, and the slot must stay held long
            // enough for it to have polled (a poll is 50 ms) before it is released.
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        let begun = Instant::now();
        let slot = StartSlot::take_from(dir.path(), 1, "test");
        let waited = begun.elapsed();
        assert!(slot.is_some());
        assert!(
            waited >= Duration::from_millis(250),
            "took the slot while it was held: {waited:?}"
        );
        release.join().unwrap();
    }

    #[test]
    fn slot_files_that_cannot_be_used_mean_no_slot_and_not_a_failed_start() {
        let dir = tempfile::tempdir().unwrap();
        // A folder where the slot file should be: opening it for writing fails.
        std::fs::create_dir(dir.path().join("slot-0")).unwrap();
        assert!(StartSlot::try_take(dir.path(), 1).is_err());
        assert!(StartSlot::take_from(dir.path(), 1, "test").is_none());
    }

    /// A port that refuses connections and that no other process can take while the socket
    /// lives (bound, never listening).
    fn unlistening() -> (tokio::net::TcpSocket, u16) {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind((LOCAL, 0).into()).unwrap();
        let port = socket.local_addr().unwrap().port();
        (socket, port)
    }

    #[test]
    fn a_configured_program_runs_and_a_missing_one_skips() {
        assert!(configured_or_skip(true, true, "V", "how"));
        assert!(!configured_or_skip(false, false, "V", "how"));
    }

    #[test]
    #[should_panic(expected = "V is set but the fixture's program is not configured: set X")]
    fn a_required_but_missing_program_fails() {
        configured_or_skip(false, true, "V", "set X");
    }

    #[test]
    #[should_panic(expected = "V=/no/such/program is not a file")]
    fn a_configured_path_that_is_no_file_fails() {
        require_file(std::ffi::OsStr::new("/no/such/program"), "V");
    }

    #[cfg(unix)]
    #[test]
    fn the_output_of_a_taken_port_is_recognised() {
        for output in [
            "[Errno 98] error while attempting to bind on address ('127.0.0.1', 40177): address already in use",
            "FATAL: Cannot open HTTP Port\n(98) Address already in use",
            "WSAEADDRINUSE",
        ] {
            assert!(is_address_in_use(output), "{output}");
        }
        assert!(!is_address_in_use("permission denied"));
    }

    #[cfg(unix)]
    #[test]
    fn a_port_taken_between_the_pick_and_the_bind_is_retried_on_another() {
        let _serial = serial();
        // The neighbour that wins the race in CI: it binds the picked port before the program does.
        let neighbour = std::net::TcpListener::bind((LOCAL, 0)).unwrap();
        let taken = neighbour.local_addr().unwrap().port();
        let mut picks = vec![taken]
            .into_iter()
            .chain(std::iter::repeat_with(free_port));
        let mut tried = Vec::new();
        let started = on_free_port(
            || picks.next().unwrap(),
            |port| {
                tried.push(port);
                // The program's own bind: what mitmproxy and Squid do after the exec.
                match std::net::TcpListener::bind((LOCAL, port)) {
                    Ok(listener) => Ok(listener),
                    Err(e) => Err(format!("[Errno 98] {e}: address already in use")),
                }
            },
        );
        assert_eq!(tried.len(), 2, "{tried:?}");
        assert_eq!(tried[0], taken);
        assert_eq!(started.local_addr().unwrap().port(), tried[1]);
        drop(neighbour);
    }

    #[cfg(unix)]
    #[test]
    #[should_panic(expected = "the fixture found its port taken 5 times")]
    fn a_port_that_stays_taken_fails_after_five_tries() {
        on_free_port(free_port, |_| -> Result<(), String> {
            Err("(98) Address already in use".into())
        });
    }

    #[cfg(unix)]
    #[test]
    #[should_panic(expected = "the fixture failed to start on port")]
    fn a_failure_that_is_not_a_taken_port_is_not_retried() {
        on_free_port(free_port, |_| -> Result<(), String> {
            Err("bad config".into())
        });
    }

    #[test]
    #[should_panic(expected = "the fixture exited early")]
    fn a_fixture_that_dies_while_another_process_answers_on_its_port_is_not_ready() {
        let _serial = serial();
        // Another process holds the port, so connecting works; the fixture itself has exited.
        let neighbour = std::net::TcpListener::bind((LOCAL, 0)).unwrap();
        let port = neighbour.local_addr().unwrap().port();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child.wait().unwrap();
        wait_ready(port, Some(&mut child), Duration::from_secs(5), String::new);
    }

    #[cfg(unix)]
    #[test]
    #[should_panic(expected = "the fixture exited early")]
    fn a_fixture_that_dies_just_after_the_port_answers_is_not_ready() {
        let _serial = serial();
        let neighbour = std::net::TcpListener::bind((LOCAL, 0)).unwrap();
        let port = neighbour.local_addr().unwrap().port();
        let mut child = Command::new("sleep").arg("0.08").spawn().unwrap();
        wait_ready(port, Some(&mut child), Duration::from_secs(5), String::new);
    }

    #[test]
    fn a_listening_port_is_ready() {
        let _serial = serial();
        let listener = std::net::TcpListener::bind((LOCAL, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        wait_ready(port, None, Duration::from_secs(5), || unreachable!());
    }

    #[test]
    #[should_panic(expected = "the fixture did not accept connections\nlog text")]
    fn a_port_nobody_listens_on_times_out_with_the_diagnostics() {
        let _serial = serial();
        let (_held, port) = unlistening();
        wait_ready(port, None, Duration::from_millis(100), || "log text".into());
    }

    #[test]
    #[should_panic(expected = "the fixture exited early")]
    fn a_fixture_that_exits_is_reported_at_once() {
        let _serial = serial();
        let (_held, port) = unlistening();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        wait_ready(port, Some(&mut child), Duration::from_secs(30), String::new);
    }

    #[test]
    fn cpu_time_is_user_plus_system_ticks_counted_after_the_name() {
        // Field 2 holds a space and a parenthesis; utime and stime are fields 14 and 15.
        let line = "42 (mitm (dump) x) S 1 42 42 0 -1 4194560 100 0 0 0 55 17 0 0 20 0 1 0 100 0 0";
        assert_eq!(parse_cpu_time(line), Some(Duration::from_millis(720)));
        assert_eq!(parse_cpu_time("no name here"), None);
        assert_eq!(parse_cpu_time("1 (x) S 1"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_busy_process_reports_more_cpu_time_than_an_idle_one() {
        let _serial = serial();
        let mut idle = Command::new("sleep").arg("30").spawn().unwrap();
        let mut busy = Command::new("sh")
            .args(["-c", "while :; do :; done"])
            .spawn()
            .unwrap();
        // A loaded host may give the loop its first 100 ms late, so wait for them.
        puddle_test_kit::eventually_blocking(
            "the busy loop gets a CPU",
            Duration::from_secs(60),
            || cpu_time(busy.id()).unwrap() >= Duration::from_millis(100),
        );
        let idle_cpu = cpu_time(idle.id()).unwrap();
        stop(&mut idle);
        stop(&mut busy);
        // flake-ok: CPU time (user plus system ticks from `/proc`), not wall time: a `sleep` spends
        // none while it waits, whatever the load (0 ns measured under 16 busy loops on 4 cores).
        assert!(idle_cpu < Duration::from_millis(100), "{idle_cpu:?}");
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn without_proc_there_is_no_cpu_time() {
        assert_eq!(cpu_time(std::process::id()), None);
    }
}
