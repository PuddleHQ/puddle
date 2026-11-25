// SPDX-License-Identifier: AGPL-3.0-or-later
//! The failure arms of the Forgejo fixture. A real Forgejo (`tests/integration/forgejo_lab.rs`)
//! never fails to start or answers wrongly on demand, so these run the fixture's own start, probes
//! and answer checks against a stand-in `forgejo` program (a shell script) that fails where the
//! test says, and against the plain values the answer checks take.
#![cfg(unix)]

use super::*;
use puddle_test_kit::{ReservedPort, quiet_port};

const NAME: &str = "git.lab.test";

/// A `forgejo` stand-in in `dir`. It answers every command the fixture runs; `fail` names the one
/// that exits 1 with "boom" (`generate:`, `migrate:`, `admin:create`, `admin:generate-access-token`,
/// `web:`), and `web` is what the `web` command does instead.
fn fake(dir: &Path, fail: &str, web: &str) -> PathBuf {
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = generate ]; then key=generate:; else shift 4; key="$1:$3"; fi
case "$key" in
  {fail}) echo boom >&2; exit 1;;
  generate:) echo "secret-$3"; exit 0;;
  admin:generate-access-token) echo lab-token; exit 0;;
  web:) {web};;
  *) exit 0;;
esac
"#
    );
    let path = dir.join("forgejo");
    puddle_test_kit::write_executable(&path, &script);
    path
}

/// What `launch` returns when the stand-in fails at `fail`.
fn launch_failing(fail: &str, web: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake(dir.path(), fail, web);
    match Forgejo::launch(bin.as_os_str(), NAME, quiet_port(), quiet_port()) {
        Ok(_) => panic!("the start succeeded although {fail} failed"),
        Err(why) => why,
    }
}

#[test]
fn a_command_forgejo_refuses_fails_the_start_with_its_output() {
    for (fail, named) in [
        ("migrate:", "forgejo migrate: boom"),
        ("admin:create", "forgejo admin user create"),
        (
            "admin:generate-access-token",
            "forgejo admin user generate-access-token",
        ),
    ] {
        let why = launch_failing(fail, "exit 0");
        assert!(why.contains(named) && why.contains("boom"), "{fail}: {why}");
    }
}

/// A Forgejo that logs its failed bind and exits fails the start with its log. Which of the two
/// causes the start reports first depends on whether the process has finished exiting when the log
/// line is first read: "exited early" when it has, "failed to start" when it has not. The fast
/// script lands on either side by scheduling; the slow one always logs first.
#[test]
fn forgejo_that_exits_before_it_answers_fails_the_start_with_its_log() {
    for web in [
        "echo 'bind: address already in use'; exit 3",
        "echo 'bind: address already in use'; sleep 0.3; exit 3",
    ] {
        let why = launch_failing("none", web);
        assert!(
            why.contains("exited early") || why.contains("failed to start"),
            "{web}: {why}"
        );
        assert!(why.contains("address already in use"), "{web}: {why}");
    }
}

#[test]
fn forgejo_that_logs_a_taken_port_and_stays_up_fails_the_start() {
    let why = launch_failing("none", "echo 'bind: address already in use'; exec sleep 60");
    assert!(why.contains("failed to start"), "{why}");
    assert!(why.contains("address already in use"), "{why}");
}

#[test]
#[should_panic(expected = "forgejo generate secret INTERNAL_TOKEN: boom")]
fn a_secret_forgejo_cannot_make_fails_the_start_at_once() {
    launch_failing("generate:", "exit 0");
}

#[test]
#[should_panic(expected = "the fixture failed to start on port")]
fn a_start_on_a_fixed_port_that_fails_panics_with_forgejos_output() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake(dir.path(), "migrate:", "exit 0");
    let _ = Forgejo::start_from(bin.as_os_str(), NAME, Some(quiet_port()));
}

#[test]
#[should_panic(expected = "the fixture failed to start on port")]
fn a_start_on_a_free_port_that_fails_for_another_reason_panics_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake(dir.path(), "migrate:", "exit 0");
    let _ = Forgejo::start_from(bin.as_os_str(), NAME, None);
}

/// The SSH port is retried on a fresh one when it is taken, as the HTTP port is. The stand-in
/// logs "address already in use" when `app.ini` names the held port as the SSH port and fails
/// with "boom" otherwise, so the panic names the port the second attempt used.
#[test]
fn a_taken_ssh_port_on_a_fixed_http_port_is_retried_on_a_fresh_one() {
    let held = quiet_port();
    let fresh = quiet_port();
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        r#"#!/bin/sh
ini="$2"; if [ "$1" = generate ]; then echo secret; exit 0; fi
case "$5" in
  web) if grep -q "SSH_PORT = {held}$" "$ini"; then echo 'bind: address already in use'; exit 3; fi; echo boom; exit 1;;
esac
exit 0
"#
    );
    let bin = dir.path().join("forgejo");
    puddle_test_kit::write_executable(&bin, &script);
    let mut picks = [held, fresh].into_iter();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Forgejo::start_picking(bin.as_os_str(), NAME, Some(quiet_port()), || {
            picks.next().unwrap()
        })
    }))
    .err()
    .expect("the start must fail on the stand-in");
    let text = caught.downcast_ref::<String>().unwrap();
    assert!(
        text.contains(&format!("failed to start on port {fresh}")),
        "{text}"
    );
}

/// A fixture value that points at nothing, so its calls fail the way a dead server's do.
fn unreachable_fixture() -> Forgejo {
    let dir = tempfile::tempdir().unwrap();
    let child = Command::new("sleep").arg("60").spawn().unwrap();
    Forgejo {
        child,
        root_pem: dir.path().join("root.pem"),
        dir,
        name: NAME.to_owned(),
        http_port: quiet_port(),
        ssh_port: quiet_port(),
        root: CertificateDer::from(Vec::new()),
        token: "secret-token".to_owned(),
        password: "secret-password".to_owned(),
        started: StartCost::default(),
    }
}

#[test]
fn a_call_to_a_server_that_is_not_there_panics_without_the_token() {
    let fixture = unreachable_fixture();
    let caught = std::panic::catch_unwind(|| fixture.api("GET", "/version", None)).unwrap_err();
    let text = caught.downcast_ref::<String>().unwrap();
    assert!(text.starts_with("curl GET /version: "), "{text}");
    assert!(!text.contains("secret-token"), "{text}");
}

#[test]
fn the_host_keys_are_the_sorted_public_key_files_and_none_without_a_data_folder() {
    let fixture = unreachable_fixture();
    assert_eq!(fixture.ssh_host_keys(), [] as [String; 0]);
    let ssh = fixture.scratch().join("data/ssh");
    fs::create_dir_all(ssh.join("unreadable.pub")).unwrap();
    fs::write(ssh.join("b.pub"), "ssh-rsa BBB host\n").unwrap();
    fs::write(ssh.join("a.pub"), "ssh-ed25519 AAA host\n").unwrap();
    fs::write(ssh.join("a"), "private").unwrap();
    assert_eq!(
        fixture.ssh_host_keys(),
        ["ssh-ed25519 AAA", "ssh-rsa BBB"].map(String::from)
    );
}

#[test]
fn ssh_greets_only_a_server_that_says_ssh() {
    let serve = |greeting: &'static [u8]| {
        let listener = std::net::TcpListener::bind((LOCAL, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::Write as _;
            let (mut conn, _) = listener.accept().unwrap();
            let _ = conn.write_all(greeting);
        });
        port
    };
    assert!(ssh_greets(serve(b"SSH-2.0-lab\r\n")));
    assert!(!ssh_greets(serve(b"HTTP/1.1")));
    assert!(!ssh_greets(serve(b"")), "a server that hangs up");
    assert!(
        !ssh_greets(ReservedPort::new().port()),
        "a refused connection"
    );
}

#[test]
fn a_status_other_than_created_panics_with_what_was_asked_and_the_answer() {
    expect_created(201, "{}", "create acme/app");
    let caught = std::panic::catch_unwind(|| expect_created(422, "name taken", "create acme/app"))
        .unwrap_err();
    let text = caught.downcast_ref::<String>().unwrap();
    assert!(text.contains("create acme/app: name taken"), "{text}");
}

#[test]
fn a_branch_answer_is_its_commit_nothing_for_404_and_a_panic_otherwise() {
    assert_eq!(
        head_from_answer(200, r#"{"commit":{"id":"abc123"}}"#, "acme/app/main"),
        Some("abc123".to_owned())
    );
    assert_eq!(head_from_answer(404, "", "acme/app/gone"), None);
    let caught =
        std::panic::catch_unwind(|| head_from_answer(500, "oops", "acme/app/main")).unwrap_err();
    let text = caught.downcast_ref::<String>().unwrap();
    assert_eq!(text, "branch acme/app/main: 500 oops");
}

#[test]
fn host_key_fingerprints_name_each_key_and_skip_lines_that_are_not_keys() {
    // Public keys of a throwaway key.
    let ed = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIL9qygvUTQlQ6FYjTLjckfijP/J7+lFVhiEa/B99Xtv1";
    let found = host_key_fingerprints(&[ed.to_owned(), "ssh-rsa notbase64".to_owned()]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, Algorithm::Ed25519);
    assert!(found[0].1.starts_with("SHA256:"), "{found:?}");
}

#[test]
fn a_private_key_file_is_readable_by_its_owner_only_and_never_overwritten() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key");
    write_private(&path, b"secret");
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    assert!(std::panic::catch_unwind(|| write_private(&path, b"again")).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"secret");
}

/// What the closure panics with.
fn panic_text(run: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let caught = std::panic::catch_unwind(run).expect_err("the call must panic");
    caught
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| caught.downcast_ref::<&str>().map(|text| (*text).to_owned()))
        .unwrap()
}

#[test]
fn a_refused_token_call_fails_with_its_status_and_leaks_neither_token_nor_password() {
    let refused = PasswordAnswer {
        status: 403,
        body: r#"{"sha1":"tok-abc123","message":"secret-password is wrong"}"#.to_owned(),
    };
    let from_token = panic_text(|| {
        refused.token("make token t");
    });
    let from_status = panic_text(|| refused.expect_status(204, "revoke token t"));
    for text in [from_token, from_status, format!("{refused:?}")] {
        assert!(text.contains("403") || text.contains("hidden"), "{text}");
        assert!(!text.contains("tok-abc123"), "{text}");
        assert!(!text.contains("secret-password"), "{text}");
    }
}

#[test]
fn a_created_token_answer_without_a_token_fails_without_its_body() {
    let odd = PasswordAnswer {
        status: 201,
        body: r#"{"other":"tok-xyz789"}"#.to_owned(),
    };
    let text = panic_text(|| {
        odd.token("make token t");
    });
    assert!(text.contains("holds no token"), "{text}");
    assert!(!text.contains("tok-xyz789"), "{text}");
}

#[test]
fn a_created_token_answer_yields_its_token() {
    let made = PasswordAnswer {
        status: 201,
        body: r#"{"sha1":"tok-abc123"}"#.to_owned(),
    };
    assert_eq!(made.token("make token t"), "tok-abc123");
}

#[test]
fn a_status_outside_the_known_ones_is_named_by_a_fixed_text() {
    let odd = PasswordAnswer {
        status: 418,
        body: String::new(),
    };
    let text = panic_text(|| odd.expect_status(204, "revoke token t"));
    assert!(text.contains("another status"), "{text}");
    assert!(!text.contains("418"), "{text}");
    assert_eq!(status_text(403), "403");
}

#[test]
fn every_known_status_is_named_by_its_own_number() {
    for status in [200, 201, 204, 400, 401, 403, 404, 409, 422, 500, 502, 503] {
        assert_eq!(status_text(status), status.to_string());
    }
}
