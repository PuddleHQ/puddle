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
