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
