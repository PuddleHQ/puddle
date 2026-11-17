// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `puddle-lab` program, run as a person would run it: the self-test, and a standing run that
//! writes its info file, serves, and stops on the stop file or on a signal.
#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "helpers outside #[test] functions fail the test by panicking; a skip says why"
)]

use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use puddle_test_kit::{MAX_WAIT, accepts, eventually_blocking};

fn lab() -> Command {
    Command::new(env!("CARGO_BIN_EXE_puddle-lab"))
}

/// Whether Squid and mitmproxy are configured; a CI job that provides them sets the flags, and a
/// missing program is then a failure.
fn programs() -> bool {
    let squid = std::env::var_os("PUDDLE_SQUID_BIN").is_some()
        || std::env::var_os("PUDDLE_SQUID_DOCKER").is_some();
    let mitm = std::env::var_os("PUDDLE_MITMDUMP_BIN").is_some();
    let required = std::env::var_os("PUDDLE_REQUIRE_SQUID").is_some()
        && std::env::var_os("PUDDLE_REQUIRE_MITMPROXY").is_some();
    assert!(
        (squid && mitm) || !required,
        "PUDDLE_REQUIRE_SQUID and PUDDLE_REQUIRE_MITMPROXY are set but a lab program is not configured"
    );
    if !(squid && mitm) {
        eprintln!("skipped: set PUDDLE_SQUID_BIN (or PUDDLE_SQUID_DOCKER) and PUDDLE_MITMDUMP_BIN");
    }
    squid && mitm
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn listening(port: u64) -> bool {
    accepts(SocketAddr::from((
        [127, 0, 0, 1],
        u16::try_from(port).unwrap(),
    )))
}

/// Starts the program with `args`, waits for its info file and returns the child and the info.
fn start(args: &[&str], dir: &Path, stop_file: bool) -> (Child, serde_json::Value) {
    let info = dir.join("lab.json");
    let mut command = lab();
    command.args(args).arg("--info").arg(&info);
    if stop_file {
        command.arg("--stop").arg(dir.join("stop"));
    }
    let child = command.stderr(Stdio::null()).spawn().unwrap();
    let text = eventually_blocking("the lab writes its info file", MAX_WAIT, || {
        std::fs::read_to_string(&info).ok()
    });
    (child, serde_json::from_str(&text).unwrap())
}

#[test]
fn no_pieces_is_a_usage_error() {
    let output = lab().output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("select at least one piece"),
        "{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("usage: puddle-lab"));
}

#[test]
fn the_self_test_checks_that_every_piece_answers() {
    if !programs() {
        return;
    }
    let output = lab().arg("--self-test").output().unwrap();
    let text = stderr(&output);
    assert!(output.status.success(), "{text}");
    for piece in ["squid on", "interceptor on", "NTLM proxy on", "PAC server"] {
        assert!(
            text.contains(&format!("ok: {piece}")),
            "{piece} missing\n{text}"
        );
    }
}

/// Whether mitmproxy is configured (and required, in a CI job that provides it).
fn mitmproxy() -> bool {
    let mitm = std::env::var_os("PUDDLE_MITMDUMP_BIN").is_some();
    assert!(
        mitm || std::env::var_os("PUDDLE_REQUIRE_MITMPROXY").is_none(),
        "PUDDLE_REQUIRE_MITMPROXY is set but PUDDLE_MITMDUMP_BIN is not"
    );
    if !mitm {
        eprintln!("skipped: set PUDDLE_MITMDUMP_BIN");
    }
    mitm
}

/// The head of the answer to one request on an open connection (`auth` is a `Proxy-Authorization`
/// value, if any); the connection stays open for the next leg.
fn ask(
    conn: &mut std::io::BufReader<std::net::TcpStream>,
    request_line: &str,
    host: &str,
    auth: Option<&str>,
) -> String {
    use std::io::{BufRead, Write};
    let header = auth.map_or_else(String::new, |a| format!("Proxy-Authorization: {a}\r\n"));
    conn.get_mut()
        .write_all(format!("{request_line} HTTP/1.1\r\nHost: {host}\r\n{header}\r\n").as_bytes())
        .unwrap();
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let read = conn.read_line(&mut line).unwrap();
        head.push_str(&line);
        if read == 0 || line == "\r\n" {
            return head;
        }
    }
}

fn connect(
    conn: &mut std::io::BufReader<std::net::TcpStream>,
    target: u16,
    auth: Option<&str>,
) -> String {
    let host = format!("127.0.0.1:{target}");
    ask(conn, &format!("CONNECT {host}"), &host, auth)
}

fn ntlm_token(kind: u32, len: usize) -> String {
    use base64::Engine as _;
    let mut message = b"NTLMSSP\0".to_vec();
    message.extend_from_slice(&kind.to_le_bytes());
    message.resize(len, 0);
    format!(
        "NTLM {}",
        base64::engine::general_purpose::STANDARD.encode(message)
    )
}

#[test]
fn a_probe_for_a_name_nobody_resolves_keeps_the_signed_in_connection_open() {
    if !mitmproxy() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut child, info) = start(&["--interceptor-ntlm"], dir.path(), true);
    let port = u16::try_from(info["interceptor"]["port"].as_u64().unwrap()).unwrap();
    let mut conn =
        std::io::BufReader::new(std::net::TcpStream::connect(("127.0.0.1", port)).unwrap());
    let probe = "HEAD http://puddle.invalid/";
    let host = "puddle.invalid";
    let demand = ask(&mut conn, probe, host, Some(&ntlm_token(1, 16)));
    assert!(
        demand.starts_with("HTTP/1.1 407")
            && demand.contains("Proxy-Authenticate: NTLM TlRMTVNTUA"),
        "{demand}"
    );
    let answer = ask(&mut conn, probe, host, Some(&ntlm_token(3, 64)));
    assert!(answer.starts_with("HTTP/1.1 502"), "{answer}");
    assert!(
        answer
            .to_ascii_lowercase()
            .contains("connection: keep-alive"),
        "the sign-in lives on this connection: {answer}"
    );
    // The connection still works for the next request, with no new sign-in.
    let next = ask(&mut conn, probe, host, None);
    assert!(next.starts_with("HTTP/1.1 502"), "{next}");
    drop(conn);
    std::fs::write(dir.path().join("stop"), "").unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn the_signing_in_interceptor_runs_the_ntlm_handshake_before_it_opens_a_tunnel() {
    if !mitmproxy() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut child, info) = start(&["--interceptor-ntlm", "--pac"], dir.path(), true);
    let interceptor = &info["interceptor"];
    assert_eq!(interceptor["sign_in"], "ntlm");
    assert_eq!(info["pac"]["proxies"].as_array().unwrap().len(), 1);
    assert!(info.get("ntlm").is_none());
    let ca = std::fs::read_to_string(interceptor["ca"].as_str().unwrap()).unwrap();
    assert!(ca.starts_with("-----BEGIN CERTIFICATE-----"), "{ca}");

    // Somewhere for the tunnel to go once the sign-in has passed.
    let origin_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let origin = origin_listener.local_addr().unwrap().port();
    let port = u16::try_from(interceptor["port"].as_u64().unwrap()).unwrap();
    let mut conn =
        std::io::BufReader::new(std::net::TcpStream::connect(("127.0.0.1", port)).unwrap());
    let demand = connect(&mut conn, origin, None);
    assert!(
        demand.starts_with("HTTP/1.1 407") && demand.contains("Proxy-Authenticate: NTLM"),
        "{demand}"
    );
    let challenge = connect(&mut conn, origin, Some(&ntlm_token(1, 16)));
    assert!(
        challenge.starts_with("HTTP/1.1 407")
            && challenge.contains("Proxy-Authenticate: NTLM TlRMTVNTUA"),
        "{challenge}"
    );
    let opened = connect(&mut conn, origin, Some(&ntlm_token(3, 64)));
    assert!(opened.starts_with("HTTP/1.1 200"), "{opened}");
    let log = std::fs::read_to_string(interceptor["log"].as_str().unwrap()).unwrap();
    assert!(
        log.contains("NTLM leg3") && log.contains("accepted"),
        "{log}"
    );

    drop(conn);
    std::fs::write(dir.path().join("stop"), "").unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn the_self_test_of_the_signing_in_interceptor_checks_the_demand_and_the_root() {
    if !mitmproxy() {
        return;
    }
    let output = lab()
        .args(["--self-test", "--interceptor-ntlm"])
        .output()
        .unwrap();
    let text = stderr(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("ok: interceptor on") && text.contains("demands NTLM"),
        "{text}"
    );
}

#[test]
fn the_self_test_of_an_open_squid_alone_checks_that_it_forwards() {
    if !programs() {
        return;
    }
    let output = lab().args(["--self-test", "--squid"]).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("ok: squid on"),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("forwards a request"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_missing_program_fails_the_self_test_and_names_the_variable() {
    let output = lab()
        .args(["--self-test", "--interceptor"])
        .env_remove("PUDDLE_MITMDUMP_BIN")
        // CI requires the program: the fixture would panic instead of reporting it missing.
        .env_remove("PUDDLE_REQUIRE_MITMPROXY")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("PUDDLE_MITMDUMP_BIN"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_standing_run_lists_its_pieces_and_stops_every_one_on_the_stop_file() {
    if !programs() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut child, info) = start(
        &[
            "--squid-basic",
            "--interceptor",
            "--ntlm",
            "--pac",
            "--squid-through",
            "interceptor",
            "--name",
            "intranet.corp.test",
        ],
        dir.path(),
        true,
    );
    assert_eq!(info["names"][0], "intranet.corp.test");
    assert_eq!(info["squid"]["auth"], "basic");
    assert_eq!(info["squid"]["user"], "alice");
    assert_ne!(info["squid"]["password"].as_str().unwrap(), "");
    assert_eq!(info["squid"]["through"], "interceptor");
    assert!(info["ntlm"]["accepts"].as_str().unwrap().contains("NTLM"));
    let ca = std::fs::read_to_string(info["interceptor"]["ca"].as_str().unwrap()).unwrap();
    assert!(ca.starts_with("-----BEGIN CERTIFICATE-----"), "{ca}");
    assert_eq!(info["pac"]["proxies"].as_array().unwrap().len(), 3);
    let ports = [
        info["squid"]["port"].as_u64().unwrap(),
        info["interceptor"]["port"].as_u64().unwrap(),
        info["ntlm"]["port"].as_u64().unwrap(),
    ];
    assert!(ports.iter().all(|port| listening(*port)), "{ports:?}");

    std::fs::write(dir.path().join("stop"), "").unwrap();
    assert!(child.wait().unwrap().success());
    for port in ports {
        eventually_blocking(
            "a stopped piece stops listening",
            Duration::from_secs(30),
            || !listening(port),
        );
    }
}

#[cfg(unix)]
#[test]
fn a_termination_signal_stops_the_lab_cleanly() {
    if !programs() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut child, info) = start(
        &["--squid", "--ntlm", "--squid-through", "ntlm", "--pac"],
        dir.path(),
        false,
    );
    let port = info["ntlm"]["port"].as_u64().unwrap();
    assert!(listening(port));
    let sent = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    assert!(child.wait().unwrap().success(), "a signal is a clean stop");
    eventually_blocking(
        "the NTLM proxy stops with the lab",
        Duration::from_secs(30),
        || !listening(port),
    );
}
