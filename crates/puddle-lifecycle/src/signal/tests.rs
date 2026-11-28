// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn control_line_names_the_cause() {
    for cause in ShutdownCause::ALL {
        let line = format!("{cause}\n");
        assert_eq!(read_request(line.as_bytes()), cause);
    }
    assert_eq!(
        read_request(&b"console-close"[..]),
        ShutdownCause::ConsoleClose,
        "no newline needed"
    );
}

#[test]
fn eof_or_garbage_means_the_front_is_gone() {
    assert_eq!(read_request(&b""[..]), ShutdownCause::FrontGone);
    assert_eq!(read_request(&b"reboot\n"[..]), ShutdownCause::FrontGone);
    assert_eq!(read_request(&b"\xff\xfe\n"[..]), ShutdownCause::FrontGone);
}

#[test]
fn only_window_close_logoff_and_system_shutdown_have_a_deadline() {
    let deadline: Vec<_> = ShutdownCause::ALL
        .into_iter()
        .filter(|c| c.is_deadline())
        .collect();
    assert_eq!(
        deadline,
        [
            ShutdownCause::ConsoleClose,
            ShutdownCause::Logoff,
            ShutdownCause::SystemShutdown
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_is_a_terminate_request() {
    let mut signals = ShutdownSignals::install().unwrap();
    let pid = std::process::id().to_string();
    let status = std::process::Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .unwrap();
    assert!(status.success());
    let cause = tokio::time::timeout(std::time::Duration::from_secs(5), signals.recv())
        .await
        .unwrap();
    assert_eq!(cause, ShutdownCause::Terminate);
}

#[test]
fn a_parked_control_read_does_not_hold_up_the_runtime() {
    // The writer stays open and silent: the read parks, as the front's pipe does after a restore.
    let (reader, _writer) = std::io::pipe().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut request = spawn_request_reader(reader).unwrap();
    let still_waiting = runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut request)
            .await
            .is_err()
    });
    assert!(still_waiting, "no request came");
    let (done, dropped) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(runtime);
        done.send(()).unwrap();
    });
    dropped
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("dropping the runtime waits for the parked read");
}

#[tokio::test]
async fn the_reader_delivers_the_request_line() {
    let request = spawn_request_reader(&b"logoff\n"[..]).unwrap();
    assert_eq!(request.await.unwrap(), ShutdownCause::Logoff);
    let request = spawn_request_reader(&b""[..]).unwrap();
    assert_eq!(request.await.unwrap(), ShutdownCause::FrontGone);
}

#[cfg(unix)]
fn send_to_self(signal: &str) {
    let pid = std::process::id().to_string();
    let status = std::process::Command::new("kill")
        .args([signal, &pid])
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(unix)]
#[tokio::test]
async fn a_second_signal_cuts_the_shutdown_short() {
    let mut signals = ShutdownSignals::install().unwrap();
    send_to_self("-TERM");
    assert_eq!(signals.recv().await, ShutdownCause::Terminate);
    let stuck = async {
        send_to_self("-HUP");
        std::future::pending::<()>().await;
    };
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        signals.until_again(stuck),
    )
    .await
    .expect("the second signal ends the wait");
    assert_eq!(second.unwrap_err(), ShutdownCause::Hangup);
}

#[cfg(unix)]
#[tokio::test]
async fn a_shutdown_that_finishes_returns_its_result() {
    let mut signals = ShutdownSignals::install().unwrap();
    assert_eq!(signals.until_again(async { 7 }).await.unwrap(), 7);
}

#[test]
fn a_repeated_interrupt_ends_the_front_but_a_repeated_deadline_event_does_not() {
    for cause in [ShutdownCause::Interrupt, ShutdownCause::Break] {
        assert!(
            !request_worker_shutdown(Some(Vec::new()), cause),
            "{cause}: first"
        );
        assert!(
            request_worker_shutdown(None::<Vec<u8>>, cause),
            "{cause}: repeated"
        );
    }
    for cause in [
        ShutdownCause::ConsoleClose,
        ShutdownCause::Logoff,
        ShutdownCause::SystemShutdown,
    ] {
        assert!(
            !request_worker_shutdown(None::<Vec<u8>>, cause),
            "{cause}: repeated"
        );
    }
}

/// The front's two Ctrl-C over a real pipe, read the way the worker reads it: the first one is
/// the worker's one request, the second finds the pipe closed and ends the front.
#[tokio::test]
async fn the_second_ctrl_c_reaches_the_worker_once_and_ends_the_front() {
    let (reader, writer) = std::io::pipe().unwrap();
    let request = spawn_request_reader(reader).unwrap();
    let mut control = Some(writer);

    assert!(!request_worker_shutdown(
        control.take(),
        ShutdownCause::Interrupt
    ));
    assert_eq!(request.await.unwrap(), ShutdownCause::Interrupt);

    assert!(control.is_none(), "the first event closed the pipe");
    assert!(request_worker_shutdown(
        control.take(),
        ShutdownCause::Interrupt
    ));
}

/// Reads the worker's request off its own standard input; the parent test below starts it.
#[cfg(windows)]
#[tokio::test]
#[ignore = "runs only as the child of `the_worker_reads_the_front_s_request_from_stdin`"]
async fn worker_child() {
    let mut signals = ShutdownSignals::install().unwrap();
    assert_eq!(signals.recv().await, ShutdownCause::Logoff);
    // the one request is the worker's only input: a second wait ends in `FrontGone`, and the
    // shutdown it covers is never cut short
    assert_eq!(signals.recv().await, ShutdownCause::FrontGone);
    assert_eq!(signals.until_again(async { 9 }).await.unwrap(), 9);
    let waiting = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        signals.until_again(std::future::pending::<()>()),
    )
    .await;
    assert!(waiting.is_err(), "the worker has no second trigger");
}

#[cfg(windows)]
#[test]
fn the_worker_reads_the_front_s_request_from_stdin() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "signal::tests::worker_child",
            "--ignored",
            "--nocapture",
        ])
        .env(crate::ROLE_VAR, "worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // flake-ok: the child installs its reader and waits for this line; it exits only after it
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"logoff\n").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
#[tokio::test]
async fn outside_the_front_the_console_events_are_installed() {
    let signals = imp::Signals::install().unwrap();
    assert!(matches!(signals, imp::Signals::Console { .. }));
}

#[cfg(windows)]
#[tokio::test]
async fn the_worker_waits_for_its_request_once() {
    let (sender, receiver) = oneshot::channel();
    let mut signals = imp::Signals::Worker(Some(receiver));
    sender.send(ShutdownCause::SystemShutdown).unwrap();
    assert_eq!(signals.recv().await, ShutdownCause::SystemShutdown);
    assert_eq!(
        signals.recv().await,
        ShutdownCause::FrontGone,
        "already read"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn a_reader_that_ended_without_a_request_is_the_front_gone() {
    let (sender, receiver) = oneshot::channel::<ShutdownCause>();
    drop(sender);
    let mut signals = imp::Signals::Worker(Some(receiver));
    assert_eq!(signals.recv().await, ShutdownCause::FrontGone);
}

#[cfg(windows)]
#[tokio::test]
async fn the_worker_has_no_second_trigger() {
    let (_sender, receiver) = oneshot::channel::<ShutdownCause>();
    let mut signals = imp::Signals::Worker(Some(receiver));
    let second =
        tokio::time::timeout(std::time::Duration::from_millis(100), signals.recv_again()).await;
    assert!(second.is_err(), "pending, whatever the front does");
}
