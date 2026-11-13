// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn control_line_names_the_cause() {
    for cause in ShutdownCause::ALL {
        let line = format!("{cause}\n");
        assert_eq!(read_request(line.as_bytes()).await, cause);
    }
    assert_eq!(
        read_request(&b"console-close"[..]).await,
        ShutdownCause::ConsoleClose,
        "no newline needed"
    );
}

#[tokio::test]
async fn eof_or_garbage_means_the_front_is_gone() {
    assert_eq!(read_request(&b""[..]).await, ShutdownCause::FrontGone);
    assert_eq!(
        read_request(&b"reboot\n"[..]).await,
        ShutdownCause::FrontGone
    );
    assert_eq!(
        read_request(&b"\xff\xfe\n"[..]).await,
        ShutdownCause::FrontGone
    );
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
