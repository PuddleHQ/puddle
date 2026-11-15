// SPDX-License-Identifier: AGPL-3.0-or-later
//! The lab's Forgejo (`puddle_e2e::lab::Forgejo`) answers like a real forge: the real `git` clones,
//! pushes and deletes a branch over HTTPS with its token, an anonymous or wrong-token client is
//! refused, and the real `ssh` signs in with a key the account holds and checks the server's host
//! key. These are the claims the VM test (`vm_radar_forgejo_*`) builds on, with no VM in between.
//!
//! Needs `PUDDLE_FORGEJO_BIN`; without it the tests skip unless `PUDDLE_REQUIRE_FORGEJO` is set
//! (the lab workflow sets it). On Windows it is Gitea's Windows release (Forgejo ships none); the
//! ssh test is Unix-only (it needs `ssh-keygen` and OpenSSH paths that Windows does not share).
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

#[cfg(unix)]
use std::fmt::Write as _;
use std::path::Path;
use std::process::Output;

use puddle_e2e::lab::{Forgejo, clean_git};

const NAME: &str = "git.lab.test";

/// `git` against `forgejo` with its root trusted and its name resolved to loopback.
fn git(forgejo: &Forgejo, cwd: &Path, args: &[&str]) -> Output {
    let root = forgejo.scratch().join("root.pem");
    let resolve = format!("{NAME}:{}:127.0.0.1", forgejo.addr().port());
    let mut command = clean_git(forgejo.scratch());
    command
        .current_dir(cwd)
        .env("GIT_SSL_CAINFO", root)
        .args(["-c", &format!("http.curloptResolve={resolve}")]);
    if cfg!(windows) {
        // The runner's Git has only Schannel, which wants a revocation answer the lab root cannot
        // give; it still verifies the chain against GIT_SSL_CAINFO. Git reads the revocation
        // switch only when the backend is named, and the system config that names it is off here.
        command.args(["-c", "http.sslBackend=schannel"]);
        command.args(["-c", "http.schannelCheckRevoke=false"]);
        // Without this Schannel ignores the CA file and trusts only the Windows store.
        command.args(["-c", "http.schannelUseSSLCAInfo=true"]);
    }
    command
        .args([
            "-c",
            "user.name=lab",
            "-c",
            "user.email=lab@example.org",
            "-c",
            "protocol.version=2",
        ])
        .args(args);
    command.output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn url(forgejo: &Forgejo, user: Option<(&str, &str)>, repo: &str) -> String {
    let auth = user.map_or_else(String::new, |(u, t)| format!("{u}:{t}@"));
    format!("https://{auth}{NAME}:{}/{repo}.git", forgejo.addr().port())
}

#[test]
fn git_clones_pushes_and_deletes_a_branch_with_the_token_and_is_refused_without_it() {
    let Some(forgejo) = Forgejo::start(NAME) else {
        return;
    };
    forgejo.add_repo("acme/app", true);
    let token = forgejo.token().to_owned();
    let work = forgejo.scratch().join("work");
    std::fs::create_dir_all(&work).unwrap();

    let with_token = url(&forgejo, Some(("x-access-token", &token)), "acme/app");
    ok(&git(&forgejo, &work, &["clone", "-q", &with_token, "app"]));
    let app = work.join("app");
    assert_eq!(
        ok(&git(&forgejo, &app, &["rev-parse", "HEAD"])),
        forgejo.branch_head("acme/app", "main").unwrap(),
        "the clone has the server's head"
    );

    std::fs::write(app.join("a.txt"), "a\n").unwrap();
    ok(&git(&forgejo, &app, &["add", "a.txt"]));
    ok(&git(&forgejo, &app, &["commit", "-q", "-m", "a"]));
    ok(&git(
        &forgejo,
        &app,
        &["push", "-q", "origin", "HEAD:refs/heads/feature"],
    ));
    assert_eq!(
        forgejo.branch_head("acme/app", "feature"),
        Some(ok(&git(&forgejo, &app, &["rev-parse", "HEAD"]))),
        "the pushed branch is at the pushed commit"
    );
    ok(&git(
        &forgejo,
        &app,
        &["push", "-q", "origin", "--delete", "feature"],
    ));
    assert_eq!(forgejo.branch_head("acme/app", "feature"), None);

    // A private repository does not answer a client with no token or a wrong one.
    for bad in [
        None,
        Some(("x-access-token", "0000000000000000000000000000000000000000")),
    ] {
        let out = git(
            &forgejo,
            &work,
            &["ls-remote", &url(&forgejo, bad, "acme/app")],
        );
        assert!(
            !out.status.success(),
            "a client with {bad:?} listed the repository"
        );
    }
}

#[cfg(unix)]
#[test]
fn ssh_signs_in_with_the_key_the_account_holds_and_checks_the_servers_host_key() {
    let Some(forgejo) = Forgejo::start(NAME) else {
        return;
    };
    forgejo.add_repo("acme/app", true);
    let dir = forgejo.scratch();
    let key = dir.join("id");
    let made = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "lab", "-f"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(made.status.success());
    forgejo.add_ssh_key(&std::fs::read_to_string(key.with_extension("pub")).unwrap());

    let port = forgejo.ssh_addr().port();
    let keys = forgejo.ssh_host_keys();
    assert!(!keys.is_empty(), "the server has no host key file");
    let known = dir.join("known_hosts");
    let mut lines = String::new();
    for key in &keys {
        writeln!(lines, "[127.0.0.1]:{port} {key}").unwrap();
    }
    std::fs::write(&known, lines).unwrap();
    let ssh = format!(
        "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile={}",
        key.display(),
        known.display()
    );
    let remote = format!("ssh://git@127.0.0.1:{port}/acme/app.git");
    let listed = clean_git(dir)
        .env("GIT_SSH_COMMAND", &ssh)
        .args(["ls-remote", &remote])
        .output()
        .unwrap();
    let text = ok(&listed);
    assert!(text.contains("refs/heads/main"), "{text}");

    // A key the account does not hold is refused.
    let other = dir.join("other");
    let made = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&other)
        .output()
        .unwrap();
    assert!(made.status.success());
    let refused = clean_git(dir)
        .env(
            "GIT_SSH_COMMAND",
            ssh.replace(&key.display().to_string(), &other.display().to_string()),
        )
        .args(["ls-remote", &remote])
        .output()
        .unwrap();
    assert!(!refused.status.success(), "an unknown key signed in");
}

#[test]
fn a_chosen_port_and_a_file_added_through_the_api_show_in_a_clone() {
    // The port a script needs fixed (443 on the nightly's runner, so a clone URL has none). A free
    // one stands in for it here; another process taking it in between fails the start, by design.
    // flake-ok: the port is only picked here, and the test is one start on it, so a clash fails loudly.
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // skip-ok: start_on fails when PUDDLE_REQUIRE_FORGEJO is set and Forgejo is missing, as start does.
    let Some(forgejo) = Forgejo::start_on(NAME, port) else {
        return;
    };
    assert_eq!(forgejo.addr().port(), port);
    forgejo.add_repo("acme/app", true);
    forgejo.add_file("acme/app", "src/main.js", "console.log(1);\n");
    assert!(
        forgejo.root_pem().is_file(),
        "the root is on disk for a store to import"
    );
    let token = forgejo.token().to_owned();
    let work = forgejo.scratch().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let with_token = url(&forgejo, Some(("x-access-token", &token)), "acme/app");
    ok(&git(&forgejo, &work, &["clone", "-q", &with_token, "app"]));
    assert_eq!(
        std::fs::read_to_string(work.join("app/src/main.js")).unwrap(),
        "console.log(1);\n"
    );
}

#[test]
fn a_stranger_on_the_chosen_port_fails_the_start_and_is_never_taken_for_forgejo() {
    // Another process holds the port Forgejo was told to bind (a parallel test's listener that was
    // handed the same free port): Forgejo exits with "address already in use" while the stranger
    // still accepts connections. The start must fail with Forgejo's own log; a start that calls
    // the stranger Forgejo returns a fixture whose API is refused once the stranger lets go.
    // flake-ok: the stranger is the point; the port is held for the whole test.
    let stranger = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = stranger.local_addr().unwrap().port();
    let started = std::panic::catch_unwind(|| Forgejo::start_on(NAME, port));
    match started {
        // Forgejo is not configured: start_on already failed the test when it is required.
        Ok(None) => {}
        Ok(Some(_)) => panic!("the start took a stranger on port {port} for Forgejo"),
        Err(panic) => {
            let why = panic
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                why.contains("address already in use") || why.contains("only one usage"),
                "the start failed, but not with Forgejo's own bind error: {why}"
            );
        }
    }
}
