// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decisions about Git LFS calls, decided by what they do.

use super::*;

// Git LFS: a batch request reads or writes by its body.

/// Go's `encoding/json` (Gitea up to 1.21 at least, checked with 1.21.11) matches a key to a field
/// ignoring case and lets a later key win, so a body with a second `operation` in another case is an
/// upload there. Only a body whose one `operation` key, in any case, says `download` is a read.
#[tokio::test]
async fn an_lfs_batch_with_a_second_operation_key_in_another_case_is_a_push() {
    let w = world();
    w.switches(OnlyListed::Push);
    let batch = "/someone/big.git/info/lfs/objects/batch";
    for body in [
        &br#"{"operation":"download","Operation":"upload","objects":[{"oid":"a","size":1}]}"#[..],
        br#"{"Operation":"upload","operation":"download","objects":[]}"#,
        br#"{"operation":"download","OPERATION":"download","objects":[]}"#,
        br#"{"operation":"download","operation":"download","objects":[]}"#,
        br#"{"OPERATION":"upload","objects":[]}"#,
        br#"{"Operation":"download","objects":[]}"#,
        br#"{"operation":"download","oper\u0041tion":"upload","objects":[]}"#,
        // A second value after the first: a reader that carries on sees the upload.
        br#"{"operation":"download","objects":[]}{"operation":"upload","objects":[]}"#,
        br#"{"operation":"download","objects":[]} x"#,
        // Not an object at all.
        br#"["operation","download"]"#,
        br#""download""#,
    ] {
        let decision = w
            .decide_with("github.com", "POST", batch, &[], Some(body))
            .await;
        assert_eq!(
            refusal(&decision).1,
            "push_denied",
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    // A key that only contains the word, or sits in an object inside, changes nothing.
    let download =
        br#"{"operation":"download","operations":"upload","ref":{"operation":"upload"},"objects":[]}"#;
    let ok = w
        .decide_with("github.com", "POST", batch, &[], Some(download))
        .await;
    assert!(injected(&ok, &token_header("CANARY-PERSONAL")), "{ok:?}");
    // Trailing white space is not a second value.
    let padded = b"{\"operation\":\"download\",\"objects\":[]}\n";
    let ok = w
        .decide_with("github.com", "POST", batch, &[], Some(padded))
        .await;
    assert!(injected(&ok, &token_header("CANARY-PERSONAL")), "{ok:?}");
}

#[tokio::test]
async fn an_lfs_batch_is_decided_by_its_operation() {
    let w = world();
    w.switches(OnlyListed::Push);
    let batch = "/someone/big.git/info/lfs/objects/batch";
    assert_eq!(w.body_wanted("github.com", "POST", batch), Some(256 * 1024));
    assert_eq!(w.body_wanted("github.com", "GET", batch), None);
    assert_eq!(w.body_wanted("github.com", "POST", FETCH), None);
    assert_eq!(
        w.body_wanted("github.com", "POST", "/acme/web/blob/main/x"),
        None
    );
    let download =
        br#"{"operation":"download","transfers":["basic"],"objects":[{"oid":"a","size":1}]}"#;
    let upload = br#"{"operation":"upload","objects":[{"oid":"a","size":1}]}"#;
    let ok = w
        .decide_with("github.com", "POST", batch, &[], Some(download))
        .await;
    assert!(injected(&ok, &token_header("CANARY-PERSONAL")), "{ok:?}");
    for body in [
        Some(&upload[..]),
        Some(&b"not json"[..]),
        Some(&b"{}"[..]),
        Some(&br#"{"operation":5}"#[..]),
        None,
    ] {
        let refused = w.decide_with("github.com", "POST", batch, &[], body).await;
        assert_eq!(refusal(&refused).1, "push_denied", "{body:?}");
    }
    // To a repository the push list names, an upload goes out.
    let own = "/acme/web.git/info/lfs/objects/batch";
    let allowed = w
        .decide_with("github.com", "POST", own, &[], Some(upload))
        .await;
    assert!(
        injected(&allowed, &token_header("CANARY-WORK")),
        "{allowed:?}"
    );
    // The other LFS calls are decided by what they do.
    for (method, target, code) in [
        (
            "PUT",
            "/someone/big.git/info/lfs/objects/abc",
            Some("push_denied"),
        ),
        (
            "POST",
            "/someone/big.git/info/lfs/objects/verify",
            Some("push_denied"),
        ),
        (
            "POST",
            "/someone/big.git/info/lfs/locks",
            Some("push_denied"),
        ),
        (
            "POST",
            "/someone/big.git/info/lfs/locks/9/unlock",
            Some("push_denied"),
        ),
        ("GET", "/someone/big.git/info/lfs/objects/abc", None),
        ("POST", "/someone/big.git/info/lfs/locks/verify", None),
        ("GET", "/someone/big.git/info/lfs/locks", None),
    ] {
        let decision = w.decide("github.com", method, target, &[]).await;
        match code {
            Some(code) => assert_eq!(refusal(&decision).1, code, "{method} {target}"),
            None => assert!(
                matches!(decision, InjectDecision::Inject(_)),
                "{method} {target}"
            ),
        }
    }
}

// Git LFS calls that write are writes whatever the method says, and the one that reads is a read
// only when it is the POST Git LFS defines.

#[tokio::test]
async fn an_lfs_verify_or_unlock_is_a_push_whatever_its_method() {
    let w = World::new();
    w.identity("Personal", "github.com", &[], true, "CANARY-PERSONAL");
    w.switches(OnlyListed::Push);
    for method in ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"] {
        for target in [
            "/someone/big.git/info/lfs/objects/verify",
            "/someone/big.git/info/lfs/locks/9/unlock",
        ] {
            let decision = w.decide("github.com", method, target, &[]).await;
            assert_eq!(refusal(&decision).1, "push_denied", "{method} {target}");
        }
    }
    assert_eq!(w.credentials.reads(), 0, "no credential was read");
    // Listed for Push, the same calls go out with the credential.
    w.list("github.com/someone/big", RepoToggles::PUSH_ONLY);
    for method in ["GET", "POST"] {
        let decision = w
            .decide(
                "github.com",
                method,
                "/someone/big.git/info/lfs/objects/verify",
                &[],
            )
            .await;
        assert!(
            injected(&decision, &token_header("CANARY-PERSONAL")),
            "{method}: {decision:?}"
        );
    }
}

#[tokio::test]
async fn an_lfs_lock_listing_is_a_pull_only_as_a_post_and_every_other_method_is_decided_by_what_it_does()
 {
    let w = World::new();
    w.identity("Personal", "github.com", &[], true, "CANARY-PERSONAL");
    w.switches(OnlyListed::Push);
    let verify = "/someone/big.git/info/lfs/locks/verify";
    let post = w.decide("github.com", "POST", verify, &[]).await;
    assert!(
        injected(&post, &token_header("CANARY-PERSONAL")),
        "{post:?}"
    );
    for method in ["PUT", "DELETE", "PATCH"] {
        let decision = w.decide("github.com", method, verify, &[]).await;
        assert_eq!(refusal(&decision).1, "push_denied", "{method}");
    }
    // A pull-only table lets the POST through and keeps the writes out.
    w.switches(OnlyListed::Both);
    w.list("github.com/someone/big", RepoToggles::PULL_ONLY);
    let post = w.decide("github.com", "POST", verify, &[]).await;
    assert!(
        injected(&post, &token_header("CANARY-PERSONAL")),
        "{post:?}"
    );
    for method in ["PUT", "DELETE"] {
        let decision = w.decide("github.com", method, verify, &[]).await;
        assert_eq!(refusal(&decision).1, "push_denied", "{method}");
    }
}
