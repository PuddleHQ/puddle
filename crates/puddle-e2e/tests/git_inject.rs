// SPDX-License-Identifier: GPL-3.0-or-later
//! Git over HTTPS end to end, tier I: real `git` against the guest's way in (the real agent on
//! Unix), the real proxy with TLS termination, the real injector over a real store, and a fake
//! Git host that is the real `git http-backend` behind TLS and records every request it answers.
//!
//! Needs `git` on the path; without it the tests skip unless `PUDDLE_REQUIRE_TOOLS` is set.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
mod rig;

use std::path::Path;

use puddle_e2e::lab::{GitHost, Versions, clean_git, git_available};
use rig::{GitRig, NAME, PERSONAL, WORK, traced_headers};

fn basic(token: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"))
    )
}

/// The commit a branch of one of the fake host's repositories points at.
fn head_of(host: &GitHost, repo: &str, branch: &str) -> String {
    let mut command = clean_git(host.scratch());
    command
        .arg("--git-dir")
        .arg(host.repo_path(repo))
        .args(["rev-parse", &format!("refs/heads/{branch}")]);
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A commit in `clone` on branch `topic`.
async fn commit(rig: &GitRig, clone: &Path, file: &str) {
    std::fs::write(clone.join(file), "guest work\n").unwrap();
    for args in [&["add", file][..], &["commit", "-q", "-m", "guest work"]] {
        let ran = rig.run(clone, args).await;
        assert!(ran.ok, "{args:?}: {}", ran.stderr);
    }
}

const TRACE: [(&str, &str); 1] = [("GIT_TRACE_CURL", "1")];

#[tokio::test]
async fn clone_fetch_push_and_ls_remote_work_and_git_never_sends_an_authorization() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    let dir = rig.work("w");
    let url = format!("https://{NAME}/acme/web.git");

    let ran = rig
        .run_env(&dir, &["clone", "-q", &url, "web"], &TRACE)
        .await;
    assert!(ran.ok, "{}", ran.stderr);
    assert!(
        traced_headers(&ran.stderr, "authorization").is_empty(),
        "git must send no Authorization of its own:\n{}",
        ran.stderr
    );
    let clone = dir.join("web");
    assert!(clone.join("README").is_file());

    let ran = rig
        .run_env(&clone, &["fetch", "-q", "origin"], &TRACE)
        .await;
    assert!(ran.ok, "{}", ran.stderr);
    assert!(traced_headers(&ran.stderr, "authorization").is_empty());

    let ran = rig.run_env(&dir, &["ls-remote", &url], &TRACE).await;
    assert!(ran.ok, "{}", ran.stderr);
    assert!(ran.stdout.contains("refs/heads/main"), "{}", ran.stdout);

    commit(&rig, &clone, "pushed.txt").await;
    let ran = rig
        .run_env(&clone, &["push", "-q", "origin", "main"], &TRACE)
        .await;
    assert!(ran.ok, "{}", ran.stderr);
    assert!(traced_headers(&ran.stderr, "authorization").is_empty());
    let local = rig.run(&clone, &["rev-parse", "HEAD"]).await.stdout;
    assert_eq!(
        head_of(&rig.host, "acme/web", "main"),
        local.trim(),
        "the push reached the host's repository"
    );

    // The host saw the same credential on every request: the work identity's, in the shape a
    // hosted service takes it. Git sent none, so the proxy added every one.
    let seen = rig.host.seen();
    assert!(seen.len() >= 6, "{seen:#?}");
    for request in &seen {
        assert_eq!(
            request.headers_named("authorization"),
            [basic(WORK)],
            "{} {}",
            request.method,
            request.target
        );
    }
    assert!(
        seen.iter()
            .any(|r| r.target.contains("git-receive-pack") && r.method == "POST"),
        "the push was a POST to git-receive-pack"
    );
    // Not one 401 round trip: git needed no retry.
    assert!(seen.iter().all(|r| r.status != 401), "{seen:#?}");

    // The audit log records the connections as injected and holds no secret.
    let records = rig.records(1).await;
    assert!(records.iter().any(|r| r["injected"] == true), "{records:?}");
    let audit = rig.audit_text();
    assert!(
        !audit.contains(WORK) && !audit.contains(PERSONAL),
        "a token reached the audit log"
    );
}

#[tokio::test]
async fn each_owner_gets_its_own_identity_and_the_rest_of_the_host_gets_the_other() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    let dir = rig.work("w");
    for (repo, token) in [
        ("acme/web", WORK),
        ("someone/else", PERSONAL),
        ("acme/open", WORK),
    ] {
        let before = rig.host.seen().len();
        let ran = rig
            .run(&dir, &["ls-remote", &format!("https://{NAME}/{repo}.git")])
            .await;
        assert!(ran.ok, "{repo}: {}", ran.stderr);
        let seen = rig.host.seen();
        assert!(seen.len() > before, "{repo}");
        for request in seen.iter().skip(before) {
            assert_eq!(
                request.headers_named("authorization"),
                [basic(token)],
                "{repo}"
            );
        }
    }
}

#[tokio::test]
async fn a_push_to_a_repository_that_is_not_listed_says_how_to_allow_it_and_nothing_reaches_the_host()
 {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    let dir = rig.work("w");
    let ran = rig
        .run(
            &dir,
            &[
                "clone",
                "-q",
                &format!("https://{NAME}/acme/unlisted.git"),
                "u",
            ],
        )
        .await;
    assert!(
        ran.ok,
        "a clone of an unlisted repository is not limited by default: {}",
        ran.stderr
    );
    let clone = dir.join("u");
    commit(&rig, &clone, "x.txt").await;
    let before = rig.host.seen().len();
    let ran = rig.run(&clone, &["push", "origin", "main"]).await;
    assert!(!ran.ok);
    assert!(
        ran.stderr.contains("not on this workspace's push list") && ran.stderr.contains("403"),
        "{}",
        ran.stderr
    );
    let after = rig.host.seen();
    assert!(
        after
            .iter()
            .skip(before)
            .all(|r| !r.target.contains("receive-pack")),
        "no push request reached the host: {after:#?}"
    );
    assert_ne!(
        head_of(&rig.host, "acme/unlisted", "main"),
        rig.run(&clone, &["rev-parse", "HEAD"]).await.stdout.trim()
    );
    // One blocked record names the refusal.
    let records = rig
        .records_where(|r| r.iter().any(|r| r["reason"] == "push_denied"))
        .await;
    let refused: Vec<_> = records
        .iter()
        .filter(|r| r["reason"] == "push_denied")
        .collect();
    assert!(!refused.is_empty(), "{records:#?}");
    assert!(
        refused
            .iter()
            .all(|r| r["decision"] == "blocked" && r["injected"] == false),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_repository_listed_for_pulls_only_clones_and_refuses_a_push() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    let dir = rig.work("w");
    let url = format!("https://{NAME}/acme/readonly.git");
    let ran = rig.run(&dir, &["clone", "-q", &url, "r"]).await;
    assert!(ran.ok, "{}", ran.stderr);
    let clone = dir.join("r");
    commit(&rig, &clone, "y.txt").await;
    let ran = rig.run(&clone, &["push", "origin", "main"]).await;
    assert!(!ran.ok);
    assert!(
        ran.stderr.contains("not on this workspace's push list"),
        "{}",
        ran.stderr
    );
}

#[tokio::test]
async fn a_revoked_token_is_a_403_that_says_so_and_not_a_login_prompt() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    rig.host.revoke_token(WORK);
    let dir = rig.work("w");
    let ran = rig
        .run(
            &dir,
            &["ls-remote", &format!("https://{NAME}/acme/web.git")],
        )
        .await;
    assert!(!ran.ok);
    assert!(ran.stderr.contains("403"), "{}", ran.stderr);
    let seen = rig.host.seen();
    assert_eq!(seen.len(), 1, "one request, one answer: {seen:#?}");
    assert_eq!(seen[0].status, 401);
}

// ---------------------------------------------------------------------------------------------
// LFS against the fake host.

const OID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

fn batch(operation: &str) -> String {
    format!(
        r#"{{"operation":"{operation}","transfers":["basic"],"objects":[{{"oid":"{OID}","size":5}}]}}"#
    )
}

const LFS_JSON: [(&str, &str); 2] = [
    ("Content-Type", "application/vnd.git-lfs+json"),
    ("Accept", "application/vnd.git-lfs+json"),
];

#[tokio::test]
async fn an_lfs_upload_to_a_push_listed_repository_carries_the_credential_and_one_that_is_not_push_listed_is_refused()
 {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    // Download: a read, the credential is added.
    let r = rig
        .request(
            "POST",
            "/acme/web.git/info/lfs/objects/batch",
            &LFS_JSON,
            batch("download").as_bytes(),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // Upload to a push-listed repository: the batch, then the object itself.
    let r = rig
        .request(
            "POST",
            "/acme/web.git/info/lfs/objects/batch",
            &LFS_JSON,
            batch("upload").as_bytes(),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        r.body
            .contains(&format!("/acme/web.git/info/lfs/objects/{OID}")),
        "{}",
        r.body
    );
    let r = rig
        .request(
            "PUT",
            &format!("/acme/web.git/info/lfs/objects/{OID}"),
            &[],
            b"hello",
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(rig.host.lfs_object(OID).as_deref(), Some(&b"hello"[..]));
    let r = rig
        .request(
            "GET",
            &format!("/acme/web.git/info/lfs/objects/{OID}"),
            &[],
            b"",
        )
        .await;
    assert_eq!((r.status, r.body.as_str()), (200, "hello"));
    for request in rig.host.seen() {
        assert_eq!(
            request.headers_named("authorization"),
            [basic(WORK)],
            "{}",
            request.target
        );
    }

    // A repository listed for pulls only: its download works, its upload is refused at the proxy
    // and the host never hears of it.
    let before = rig.host.seen().len();
    let r = rig
        .request(
            "POST",
            "/acme/readonly.git/info/lfs/objects/batch",
            &LFS_JSON,
            batch("download").as_bytes(),
        )
        .await;
    assert_eq!(r.status, 200);
    let r = rig
        .request(
            "POST",
            "/acme/readonly.git/info/lfs/objects/batch",
            &LFS_JSON,
            batch("upload").as_bytes(),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(r.body.contains("push list"), "{}", r.body);
    let r = rig
        .request(
            "PUT",
            &format!("/acme/readonly.git/info/lfs/objects/{OID}"),
            &[],
            b"nope",
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    let after = rig.host.seen();
    assert_eq!(
        after.len(),
        before + 1,
        "only the download reached the host: {after:#?}"
    );
    assert!(
        rig.host.lfs_object(OID).as_deref() == Some(&b"hello"[..]),
        "the refused PUT stored nothing"
    );
}

// ---------------------------------------------------------------------------------------------
// The hostile cases, from the fake host's point of view: it must see nothing it should not.

#[tokio::test]
async fn hg15_a_request_for_another_host_or_a_wrong_sni_never_reaches_the_host_with_a_credential() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    // A wrong SNI: a TLS alert, nothing upstream.
    let get = format!(
        "GET /acme/web.git/info/refs HTTP/1.1\r\nHost: {NAME}\r\nConnection: close\r\n\r\n"
    );
    assert!(
        rig.https_raw(&format!("{NAME}:443"), "other.test", get.as_bytes())
            .await
            .is_err(),
        "an SNI that is not the CONNECT name is refused"
    );
    // A Host header for another name on the right SNI: 421, nothing upstream.
    let other =
        "GET /acme/web.git/info/refs HTTP/1.1\r\nHost: other.test\r\nConnection: close\r\n\r\n";
    let r = rig
        .https_raw(&format!("{NAME}:443"), NAME, other.as_bytes())
        .await
        .unwrap();
    assert_eq!(r.status, 421);
    // An absolute-form target for another host: the same.
    let absolute = format!(
        "GET https://other.test/acme/web.git/info/refs HTTP/1.1\r\nHost: {NAME}\r\nConnection: close\r\n\r\n"
    );
    let r = rig
        .https_raw(&format!("{NAME}:443"), NAME, absolute.as_bytes())
        .await
        .unwrap();
    assert_eq!(r.status, 421);
    // The same name on another port is spliced, not decrypted: the client sees the host's own
    // certificate (not the workspace's), and the host sees no Authorization.
    let mut head = format!(
        "GET /acme/open.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: {NAME}\r\nConnection: close\r\n\r\n"
    );
    let refused = rig
        .https_raw(&format!("{NAME}:8443"), NAME, head.as_bytes())
        .await
        .unwrap_err();
    assert!(
        refused.contains("UnknownIssuer"),
        "the workspace CA must not vouch for it: {refused}"
    );
    head.clear();
    let get = format!(
        "GET /acme/open.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: {NAME}\r\nConnection: close\r\n\r\n"
    );
    let spliced = rig
        .https_raw_with(
            &format!("{NAME}:8443"),
            NAME,
            get.as_bytes(),
            std::slice::from_ref(rig.host.root()),
        )
        .await
        .unwrap();
    assert_eq!(spliced.status, 200);
    let seen = rig.host.seen();
    assert_eq!(
        seen.len(),
        1,
        "only the spliced request reached the host: {seen:#?}"
    );
    assert!(
        seen[0].headers_named("authorization").is_empty(),
        "{seen:#?}"
    );
}

#[tokio::test]
async fn hg16_ambiguous_framing_is_refused_before_anything_goes_upstream() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    let authority = format!("{NAME}:443");
    for (what, raw) in [
        (
            "content-length and transfer-encoding",
            format!(
                "POST /acme/web.git/git-upload-pack HTTP/1.1\r\nHost: {NAME}\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"
            ),
        ),
        (
            "an obfuscated transfer-encoding",
            format!(
                "POST /acme/web.git/git-upload-pack HTTP/1.1\r\nHost: {NAME}\r\nTransfer-Encoding: xchunked\r\nContent-Length: 4\r\n\r\n0000"
            ),
        ),
        (
            "a duplicate Host",
            format!(
                "GET /acme/web.git/info/refs HTTP/1.1\r\nHost: {NAME}\r\nHost: other.test\r\n\r\n"
            ),
        ),
    ] {
        let r = rig
            .https_raw(&authority, NAME, raw.as_bytes())
            .await
            .unwrap();
        assert_eq!(r.status, 400, "{what}: {}", r.body);
    }
    assert!(rig.host.seen().is_empty(), "{:#?}", rig.host.seen());
}

#[tokio::test]
async fn hg17_the_workspaces_own_authorization_goes_out_as_sent_and_nothing_is_added() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    let r = rig
        .request(
            "GET",
            "/acme/web.git/info/refs?service=git-upload-pack",
            &[("Authorization", "Bearer mine")],
            b"",
        )
        .await;
    assert_eq!(
        r.status, 401,
        "the host's own answer to a token it does not know"
    );
    let seen = rig.host.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].headers_named("authorization"), ["Bearer mine"]);
    // Proxy-Authorization is the proxy's and never travels.
    let r = rig
        .request(
            "GET",
            "/acme/open.git/info/refs?service=git-upload-pack",
            &[("Proxy-Authorization", "Basic c2VjcmV0OnNlY3JldA==")],
            b"",
        )
        .await;
    assert_eq!(r.status, 200);
    assert!(
        rig.host
            .seen()
            .last()
            .unwrap()
            .headers_named("proxy-authorization")
            .is_empty()
    );
}

#[tokio::test]
async fn hg18_a_push_to_an_unlisted_repository_by_a_path_trick_is_refused_and_never_goes_upstream()
{
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    let service = "info/refs?service=git-receive-pack";
    let ambiguous = [
        format!("/acme/web.git/../unlisted.git/{service}"),
        format!("/acme/./unlisted.git/{service}"),
        format!("/acme%2Funlisted.git/{service}"),
        format!("/acme//unlisted.git/{service}"),
        format!("/acme/unlisted.git//{service}"),
        format!("/acme\\unlisted.git/{service}"),
        format!("/acme/%75nlisted.git/{service}"),
        format!("/acme/unlisted.git%00/{service}"),
        format!("/acme/web.git;x=../unlisted.git/{service}"),
    ];
    for path in &ambiguous {
        let r = rig.request("GET", path, &[], b"").await;
        assert_eq!(r.status, 400, "{path}: {}", r.body);
    }
    // Case and the suffix do not make another repository: the same refusal as the plain name.
    for path in [
        format!("/ACME/Unlisted.GIT/{service}"),
        format!("/acme/unlisted/{service}"),
    ] {
        let r = rig.request("GET", &path, &[], b"").await;
        assert_eq!(r.status, 403, "{path}: {}", r.body);
        assert!(r.body.contains("push list"), "{}", r.body);
    }
    assert!(
        rig.host.seen().is_empty(),
        "nothing reached the host: {:#?}",
        rig.host.seen()
    );
}

#[tokio::test]
async fn hg19_a_host_whose_certificate_names_another_host_is_a_502_and_no_credential_is_sent() {
    if !git_available() {
        return;
    }
    let rig = GitRig::with_certificate(Versions::Http1, "other.example").await;
    let dir = rig.work("w");
    let ran = rig
        .run(
            &dir,
            &["ls-remote", &format!("https://{NAME}/acme/web.git")],
        )
        .await;
    assert!(!ran.ok);
    assert!(ran.stderr.contains("502"), "{}", ran.stderr);
    assert!(
        rig.host.seen().is_empty(),
        "the host never got a request: {:#?}",
        rig.host.seen()
    );
}

#[tokio::test]
async fn hg20_a_redirect_to_another_host_is_passed_on_and_never_followed() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    let r = rig
        .request(
            "GET",
            "/acme/moved.git/info/refs?service=git-upload-pack",
            &[],
            b"",
        )
        .await;
    assert_eq!(r.status, 302);
    assert_eq!(
        r.header("location"),
        Some("https://evil.test/acme/web.git/info/refs")
    );
    assert_eq!(rig.host.seen().len(), 1, "the proxy did not follow it");
}

// ---------------------------------------------------------------------------------------------
// HTTP/2 on a decrypted host.

#[tokio::test]
async fn git_over_http2_on_a_decrypted_host_gets_the_credential_on_every_request() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Both).await;
    let dir = rig.work("w");
    let url = format!("https://{NAME}/acme/web.git");
    let ran = rig
        .run_env(
            &dir,
            &["-c", "http.version=HTTP/2", "clone", "-q", &url, "web"],
            &TRACE,
        )
        .await;
    assert!(ran.ok, "{}", ran.stderr);
    assert!(
        ran.stderr.contains("HTTP/2") || ran.stderr.contains("h2"),
        "git must have spoken HTTP/2 to the proxy:\n{}",
        ran.stderr
            .lines()
            .filter(|l| l.contains("ALPN") || l.contains("HTTP/"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let clone = dir.join("web");
    commit(&rig, &clone, "h2.txt").await;
    let ran = rig
        .run(
            &clone,
            &["-c", "http.version=HTTP/2", "push", "-q", "origin", "main"],
        )
        .await;
    assert!(ran.ok, "{}", ran.stderr);
    let seen = rig.host.seen();
    assert!(
        seen.iter().all(|r| r.version == "HTTP/2"),
        "every upstream request is HTTP/2: {:?}",
        seen.iter().map(|r| r.version).collect::<Vec<_>>()
    );
    for request in &seen {
        assert_eq!(request.headers_named("authorization"), [basic(WORK)]);
    }
}

#[tokio::test]
async fn control_the_trace_check_sees_an_authorization_git_does_send() {
    if !git_available() {
        return;
    }
    let rig = GitRig::new(Versions::Http1).await;
    let dir = rig.work("w");
    let ran = rig
        .run_env(
            &dir,
            &[
                "-c",
                "http.extraHeader=Authorization: Bearer mine",
                "ls-remote",
                &format!("https://{NAME}/acme/open.git"),
            ],
            &TRACE,
        )
        .await;
    assert!(ran.ok, "{}", ran.stderr);
    assert!(
        !traced_headers(&ran.stderr, "authorization").is_empty(),
        "the trace check must see a header git sends:\n{}",
        ran.stderr
    );
    // git's own header goes out as sent; the proxy added nothing beside it.
    let seen = rig.host.seen();
    assert!(
        seen.iter()
            .all(|r| r.headers_named("authorization") == ["Bearer mine"]),
        "{seen:#?}"
    );
}
