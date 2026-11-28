// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decisions about a source that cannot supply its secret.

use super::*;

// A source that cannot supply its secret.

#[tokio::test]
async fn a_push_with_a_source_that_needs_a_sign_in_is_a_502_that_says_so_and_never_a_401() {
    tokio::time::pause();
    let w = world();
    let work = &w.store.identities(puddle_store::UserId::OWNER).unwrap()[0];
    let source = work.credentials[0].source.clone();
    w.credentials.fail(&source, SourceError::NotSignedIn);
    for (method, target) in [
        ("GET", PUSH_ADVERT),
        ("POST", "/acme/web.git/git-receive-pack"),
    ] {
        let decision = w.decide("github.com", method, target, &[]).await;
        let (status, code, message) = refusal(&decision);
        assert_eq!((status, code), (502, "credential_unavailable"), "{target}");
        assert!(message.contains("Work"), "{message}");
        assert!(message.contains("not signed in"), "{message}");
        assert!(message.contains("sign in to it in Puddle"), "{message}");
        assert!(message.contains("t-work"), "names the source: {message}");
    }
    // The source's cache raises the sign-in notice, not the injector: no second one.
    assert_eq!(w.events.take(), []);
    // The same request with the workspace's own header does not need the source.
    assert!(matches!(
        w.decide(
            "github.com",
            "POST",
            "/acme/web.git/git-receive-pack",
            &[OWN]
        )
        .await,
        InjectDecision::PassThroughOwnCredential
    ));
}

#[tokio::test]
async fn a_read_with_a_source_that_needs_a_sign_in_goes_out_with_no_credential() {
    tokio::time::pause();
    let w = world();
    let work = &w.store.identities(puddle_store::UserId::OWNER).unwrap()[0];
    let source = work.credentials[0].source.clone();
    for error in [SourceError::NotSignedIn, SourceError::Timeout(Tool::Gh)] {
        w.credentials.fail(&source, error);
        for (method, target) in [
            ("GET", FETCH),
            ("POST", "/acme/web.git/git-upload-pack"),
            ("GET", "/acme/web.git/info/lfs/objects/abc"),
            ("POST", "/acme/web.git/info/lfs/locks/verify"),
        ] {
            let decision = w.decide("github.com", method, target, &[]).await;
            let InjectDecision::PassThroughNoting(unauthorized) = decision else {
                panic!("{method} {target}: {decision:?}");
            };
            // A private repository's `401` is the sign-in refusal, never the server's `401`.
            let replaced = unauthorized.refusal().expect("replaced");
            assert_eq!(replaced.status(), 502);
            assert_eq!(replaced.code(), ReasonCode::CredentialUnavailable);
            assert!(replaced.message().contains("sign in to it in Puddle"));
            assert!(replaced.message().contains("Work"));
        }
    }
    // An LFS download batch is a read too, an upload is not.
    let download = br#"{"operation":"download","objects":[]}"#;
    let upload = br#"{"operation":"upload","objects":[]}"#;
    let batch = "/acme/web.git/info/lfs/objects/batch";
    let ok = w
        .decide_with("github.com", "POST", batch, &[], Some(download))
        .await;
    assert!(matches!(ok, InjectDecision::PassThroughNoting(_)), "{ok:?}");
    for body in [Some(&upload[..]), Some(&b"not json"[..]), None] {
        let refused = w.decide_with("github.com", "POST", batch, &[], body).await;
        assert_eq!(refusal(&refused).1, "credential_unavailable", "{body:?}");
    }
    // No notice from the injector, and no secret was read twice over for nothing.
    assert_eq!(w.events.take(), []);
}

#[tokio::test]
async fn a_request_that_reads_like_a_read_but_writes_never_goes_out_anonymously() {
    tokio::time::pause();
    let w = world();
    let work = &w.store.identities(puddle_store::UserId::OWNER).unwrap()[0];
    w.credentials
        .fail(&work.credentials[0].source, SourceError::NotSignedIn);
    for (method, target) in [
        ("GET", PUSH_ADVERT),
        (
            "GET",
            "/acme/web.git/info/refs?service=git-upload-pack&service=git-receive-pack",
        ),
        ("GET", "/acme/web.git/git-receive-pack"),
        ("HEAD", "/acme/web.git/git-receive-pack"),
        ("POST", "/acme/web.git/git-receive-pack"),
        ("PUT", "/acme/web.git/git-upload-pack"),
        ("DELETE", "/acme/web.git/info/refs?service=git-upload-pack"),
        ("PATCH", "/acme/web.git/info/refs"),
        ("PUT", "/acme/web.git/info/lfs/objects/abc"),
        ("POST", "/acme/web.git/info/lfs/objects/verify"),
        ("POST", "/acme/web.git/info/lfs/locks"),
        ("POST", "/acme/web.git/info/lfs/locks/9/unlock"),
        ("GET", "/acme/web.git/info/lfs/git-receive-pack"),
    ] {
        let decision = w.decide("github.com", method, target, &[]).await;
        assert!(
            matches!(decision, InjectDecision::Refuse(_)),
            "{method} {target}: {decision:?}"
        );
    }
    // A path that names the repository two ways is refused before anything else.
    let decision = w
        .decide("github.com", "GET", "/acme/web.git/../x.git/info/refs", &[])
        .await;
    assert_eq!(refusal(&decision).1, "bad_git_path");
}

#[tokio::test]
async fn the_repository_table_still_gates_an_anonymous_read() {
    tokio::time::pause();
    let w = world();
    w.switches(OnlyListed::Both);
    let work = &w.store.identities(puddle_store::UserId::OWNER).unwrap()[0];
    w.credentials
        .fail(&work.credentials[0].source, SourceError::NotSignedIn);
    // Not on the pull list: refused by the table before the credential is asked for.
    w.list("github.com/acme/closed", RepoToggles::NEITHER);
    let decision = w
        .decide(
            "github.com",
            "GET",
            "/acme/closed.git/info/refs?service=git-upload-pack",
            &[],
        )
        .await;
    assert_eq!(refusal(&decision).1, "pull_denied");
}

#[tokio::test]
async fn an_anonymous_read_carries_no_header_of_puddles() {
    tokio::time::pause();
    let w = world();
    let work = &w.store.identities(puddle_store::UserId::OWNER).unwrap()[0];
    w.credentials
        .fail(&work.credentials[0].source, SourceError::NotSignedIn);
    let decision = w.decide("github.com", "GET", FETCH, &[]).await;
    let InjectDecision::PassThroughNoting(unauthorized) = decision else {
        panic!("{decision:?}");
    };
    let text = format!(
        "{:?}",
        unauthorized.refusal().map(|r| r.message().to_owned())
    );
    assert!(!text.contains("CANARY"), "{text}");
}

#[tokio::test]
async fn a_push_with_a_broken_source_is_a_502_without_a_sign_in_notice_and_a_read_goes_out_anonymously()
 {
    let w = world();
    let work = &w.store.identities(puddle_store::UserId::OWNER).unwrap()[0];
    w.credentials.fail(
        &work.credentials[0].source,
        SourceError::ToolMissing(Tool::Gh),
    );
    let (status, code, message) = refusal(&w.decide("github.com", "GET", PUSH_ADVERT, &[]).await);
    assert_eq!((status, code), (502, "credential_unavailable"));
    assert!(message.contains("is not installed"), "{message}");
    assert!(message.contains("check it in Puddle"), "{message}");
    // A read needs no credential from a source that is not there: it goes out without one.
    let read = w.decide("github.com", "GET", FETCH, &[]).await;
    let InjectDecision::PassThroughNoting(unauthorized) = read else {
        panic!("{read:?}");
    };
    let replaced = unauthorized.refusal().expect("replaced");
    assert!(replaced.message().contains("is not installed"));
    assert!(replaced.message().contains("check it in Puddle"));
    assert_eq!(w.events.take(), []);
}

/// Whatever stops the source, a read goes out with nothing Puddle holds, and a push is the `502`.
#[tokio::test]
async fn every_kind_of_source_failure_lets_a_read_go_out_without_a_credential() {
    for err in [
        SourceError::NotSignedIn,
        SourceError::Timeout(Tool::Gh),
        SourceError::ToolMissing(Tool::Gh),
        SourceError::CouldNotRun(Tool::Gh),
        SourceError::WrongTenant,
        SourceError::NoAccess,
        SourceError::StoreUnavailable,
        SourceError::Refused(puddle_secrets::Refusal::WrongTarget),
        SourceError::Refused(puddle_secrets::Refusal::Malformed),
        SourceError::Refused(puddle_secrets::Refusal::Unreadable),
    ] {
        let w = world();
        let work = &w.store.identities(puddle_store::UserId::OWNER).unwrap()[0];
        w.credentials.fail(&work.credentials[0].source, err.clone());
        let read = w.decide("github.com", "GET", FETCH, &[]).await;
        let InjectDecision::PassThroughNoting(unauthorized) = read else {
            panic!("{err:?}: {read:?}");
        };
        // A private repository's `401` becomes a refusal that names why the source failed.
        let refused = unauthorized.refusal().expect("replaced");
        assert!(
            refused.message().contains(&err.to_string()),
            "{err:?}: {}",
            refused.message()
        );
        let push = w.decide("github.com", "GET", PUSH_ADVERT, &[]).await;
        assert_eq!(refusal(&push).1, "credential_unavailable", "{err:?}");
    }
}

#[tokio::test]
async fn a_token_that_only_looks_like_an_entra_token_never_reaches_a_header_raw() {
    let w = World::new();
    // Only a plain JWT goes as a Bearer, which shows the token raw; this one has a byte a header
    // cannot carry and goes as a personal access token, wrapped in Basic's base64.
    let ado = w.identity("Ado", "dev.azure.com", &["contoso"], false, "x");
    w.credentials.give(&ado.source, "eyJa.b\u{7f}.c");
    let decision = w
        .decide("dev.azure.com", "GET", "/contoso/p/_git/r/info/refs", &[])
        .await;
    assert!(
        injected(&decision, &basic("", "eyJa.b\u{7f}.c")),
        "{decision:?}"
    );
}

#[tokio::test]
async fn a_credential_for_somewhere_else_is_never_sent() {
    let w = World::new();
    // A hand-edited store row: the source is for another host than the credential's.
    let made = w.identity("Work", "github.com", &["acme"], false, "CANARY-WORK");
    let mut identity = w
        .store
        .identity(puddle_store::UserId::OWNER, made.id)
        .unwrap();
    let mut json = serde_json::to_value(&identity.credentials).unwrap();
    json[0]["source"] = serde_json::json!({
        "kind": "stored", "id": "t-work", "scope": {"host": "gitlab.example"}
    });
    identity.credentials = serde_json::from_value(json).unwrap();
    let mut git = WorkspaceGit::unconfigured();
    git.identities = vec![identity];
    git.only_push_listed = false;
    let injector = GitInjector::new(
        w.workspace.clone(),
        StaticSettings::new(git),
        w.credentials.clone(),
        w.events.clone(),
    );
    let host = Host::parse_normalised("github.com").unwrap();
    let context = InjectContext {
        workspace: &w.workspace,
        host: &host,
    };
    let lines = vec!["host: github.com".to_owned()];
    let view = RequestView::new("GET", "/acme/web.git/info/refs", &lines);
    let decision = injector.decide(&context, &view).await;
    let (status, code, message) = refusal(&decision);
    assert_eq!((status, code), (502, "credential_unavailable"));
    assert!(message.contains("is not for github.com/acme"), "{message}");
    assert_eq!(w.credentials.reads(), 0);
    let _: &SourceSpec = &made.source;
}

#[tokio::test]
async fn settings_that_cannot_be_read_are_a_502_and_nothing_goes_out() {
    let w = World::new();
    let settings = StaticSettings::new(WorkspaceGit::unconfigured());
    settings.fail("the database is locked");
    let injector = GitInjector::new(
        w.workspace.clone(),
        settings,
        w.credentials.clone(),
        w.events.clone(),
    );
    let host = Host::parse_normalised("github.com").unwrap();
    let context = InjectContext {
        workspace: &w.workspace,
        host: &host,
    };
    let lines = vec!["host: github.com".to_owned()];
    let view = RequestView::new("GET", "/acme/web.git/info/refs", &lines);
    let (status, code, message) = refusal(&injector.decide(&context, &view).await);
    assert_eq!((status, code), (502, "git_settings_unavailable"));
    assert!(message.contains("the database is locked"), "{message}");
    // A path that is not Git never asks for the settings.
    let lines = vec!["host: github.com".to_owned()];
    let view = RequestView::new("GET", "/login", &lines);
    assert!(matches!(
        injector.decide(&context, &view).await,
        InjectDecision::PassThrough
    ));
}

#[tokio::test]
async fn no_refusal_or_log_line_carries_a_secret() {
    let w = world();
    let mut seen = String::new();
    for (method, target, headers) in [
        ("GET", FETCH, &[][..]),
        ("POST", "/acme/other.git/git-receive-pack", &[][..]),
        ("POST", "/acme/%77eb.git/git-receive-pack", &[][..]),
        ("GET", "/someone/else.git/info/refs", &[][..]),
    ] {
        let decision = w.decide("github.com", method, target, headers).await;
        let _ = write!(seen, "{decision:?}");
    }
    let work = &w.store.identities(puddle_store::UserId::OWNER).unwrap()[0];
    w.credentials
        .fail(&work.credentials[0].source, SourceError::NotSignedIn);
    let refused = w.decide("github.com", "GET", FETCH, &[]).await;
    let _ = write!(seen, "{refused:?}{:?}", w.events.events());
    assert!(!seen.contains("CANARY"), "{seen}");
}

#[test]
fn the_injector_prints_no_settings() {
    let w = World::default();
    assert_eq!(
        format!("{:?}", w.injector),
        "GitInjector { workspace: WorkspaceId(\"box\"), .. }"
    );
}

#[tokio::test]
async fn forgetting_and_reading_go_through_the_credential_source() {
    let w = world();
    let work = &w.store.identities(puddle_store::UserId::OWNER).unwrap()[0];
    let source = &work.credentials[0].source;
    let before = w.credentials.reads();
    let _ = w.decide("github.com", "GET", FETCH, &[]).await;
    assert_eq!(w.credentials.reads(), before + 1);
    CredentialSource::forget(&*w.credentials, source);
    assert_eq!(w.credentials.forgotten(), std::slice::from_ref(source));
}

#[tokio::test]
async fn a_flood_of_different_refusals_raises_a_bounded_number_of_notices() {
    tokio::time::pause();
    let w = world();
    for n in 0..300 {
        let target = format!("/acme/other{n}.git/git-receive-pack");
        let decision = w.decide("github.com", "POST", &target, &[]).await;
        assert_eq!(refusal(&decision).1, "push_denied");
    }
    assert_eq!(w.events.take().len(), 256, "the notices stop at the bound");
    // Past their window the old ones are forgotten and new ones are raised again.
    tokio::time::advance(Duration::from_secs(11)).await;
    let _ = w
        .decide(
            "github.com",
            "POST",
            "/acme/another.git/git-receive-pack",
            &[],
        )
        .await;
    assert_eq!(w.events.take().len(), 1);
}

#[tokio::test]
async fn settings_a_test_changes_by_hand_apply_to_the_next_request() {
    let w = World::new();
    let settings = StaticSettings::new(WorkspaceGit::unconfigured());
    let injector = GitInjector::new(
        w.workspace.clone(),
        settings.clone(),
        w.credentials.clone(),
        w.events.clone(),
    );
    let host = Host::parse_normalised("github.com").unwrap();
    let context = InjectContext {
        workspace: &w.workspace,
        host: &host,
    };
    let lines = vec!["host: github.com".to_owned()];
    let view = RequestView::new("POST", "/acme/web.git/git-receive-pack", &lines);
    // Nothing listed and the push list on by default.
    let first = injector.decide(&context, &view).await;
    assert_eq!(refusal(&first).1, "push_denied");
    let mut git = WorkspaceGit::unconfigured();
    git.only_push_listed = false;
    settings.set(git);
    let second = injector.decide(&context, &view).await;
    assert!(
        matches!(second, InjectDecision::PassThroughNoting(_)),
        "{second:?}"
    );
}
