// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_root_certificate_is_pem() {
    assert!(
        Root::generate(&Prefix::new("t"))
            .pem()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
}

#[test]
fn the_port_mitmproxy_bound_is_read_from_its_log() {
    let log = "[11:29:01.892] HTTP(S) proxy listening at 127.0.0.1:39551.\n[11:29:02] more";
    assert_eq!(listening_port(log), Some(39551));
    assert_eq!(listening_port("[11:29:01.892] loading addons"), None);
    assert_eq!(listening_port("proxy listening at nowhere."), None);
}

#[cfg(unix)]
#[test]
fn the_logged_port_is_returned_while_the_child_runs() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    fs::write(&log, "HTTP(S) proxy listening at 127.0.0.1:4242.\n").unwrap();
    let mut child = Command::new("sleep").arg("5").spawn().unwrap();
    let port = wait_listening(&mut child, &log, Duration::from_secs(5));
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(port, 4242);
}

#[cfg(unix)]
#[test]
#[should_panic(expected = "mitmproxy exited early")]
fn a_mitmproxy_that_exits_before_logging_its_port_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    fs::write(&log, "Error logged during startup, exiting...\n").unwrap();
    let mut child = Command::new("true").spawn().unwrap();
    wait_listening(&mut child, &log, Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn a_mitmproxy_that_logs_no_port_in_time_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_listening(&mut child, &log, Duration::from_millis(200));
    }));
    let message = *outcome.unwrap_err().downcast::<String>().unwrap();
    assert!(
        message.contains("mitmproxy did not log its port: not true within 200ms"),
        "{message}"
    );
    let pid = child.id();
    assert!(
        message.contains(&format!("sleep pid {pid} started +0.00s CPU")),
        "{message}"
    );
    assert!(
        message.contains("a trivial program (true) took"),
        "{message}"
    );
    assert!(
        child.try_wait().unwrap().is_some(),
        "the child that never logged is stopped, not left running"
    );
}

#[test]
fn an_interceptor_binds_its_own_port_and_nobody_else_can_take_it() {
    // `start` panics when `PUDDLE_REQUIRE_MITMPROXY` is set (CI) and prints `skipped:` and
    // gives `None` otherwise, so a host without mitmdump skips visibly and CI cannot.
    let Some(interceptor) = Interceptor::start(&[]) else {
        return;
    };
    let port = interceptor.port();
    assert_ne!(port, 0);
    let taken = std::net::TcpListener::bind((LOCAL, port)).unwrap_err();
    assert_eq!(taken.kind(), std::io::ErrorKind::AddrInUse);
}

#[test]
fn handshake_log_lines_parse_into_legs() {
    let lines = [
        "[10:00:00.1] NTLM 407 a1b2c3d4 echo.corp.test",
        "[10:00:00.2] NTLM leg1 a1b2c3d4 -> challenge TlRMTVNTUAAC",
        "[10:00:00.3] NTLM leg3 a1b2c3d4 accepted user=LAB\\alice ntlmv2=True",
        "[10:00:00.4] unrelated line",
        "[10:00:00.5] NTLM leg9 a1b2c3d4 -> something new",
    ];
    let legs: Vec<_> = lines.iter().filter_map(|l| parse_leg(l)).collect();
    assert_eq!(
        legs,
        vec![
            NtlmLeg::Demand {
                conn: "a1b2c3d4".into()
            },
            NtlmLeg::Challenge {
                conn: "a1b2c3d4".into()
            },
            NtlmLeg::Accepted {
                conn: "a1b2c3d4".into(),
                user: "LAB\\alice".into(),
                ntlmv2: true
            },
        ]
    );
}
