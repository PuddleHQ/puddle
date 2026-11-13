// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (waiter_begins, begins) = std::sync::mpsc::channel();
    let release = std::thread::spawn({
        let released = released.clone();
        move || {
            begins.recv().unwrap();
            // Only gives the waiter time to poll while the slot is held; nothing asserts
            // on it, so a stalled runner can't fail the test.
            std::thread::sleep(Duration::from_millis(200));
            released.store(true, Ordering::SeqCst);
            drop(held);
        }
    });
    waiter_begins.send(()).unwrap();
    let slot = StartSlot::take_from(dir.path(), 1, "test");
    assert!(slot.is_some());
    assert!(
        released.load(Ordering::SeqCst),
        "took the slot while it was held"
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
#[cfg(any(target_os = "linux", windows))]
fn a_busy_process_reports_more_cpu_time_than_an_idle_one() {
    let _serial = serial();
    let quiet = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let (program, args) = procs::programs::IDLE;
    let mut idle = quiet(program, args);
    let (program, args) = procs::programs::BUSY;
    let mut busy = quiet(program, args);
    // A loaded host may give the loop its first 100 ms late, so wait for them.
    puddle_test_kit::eventually_blocking(
        "the busy loop gets a CPU",
        Duration::from_secs(60),
        || procs::tree_cpu(busy.id()).unwrap() >= Duration::from_millis(100),
    );
    let idle_cpu = procs::tree_cpu(idle.id()).unwrap();
    stop(&mut idle);
    stop(&mut busy);
    // flake-ok: CPU time (user plus system time from the OS), not wall time: a `sleep` spends
    // none while it waits, whatever the load (0 ns measured under 16 busy loops on 4 cores).
    assert!(idle_cpu < Duration::from_millis(100), "{idle_cpu:?}");
}
