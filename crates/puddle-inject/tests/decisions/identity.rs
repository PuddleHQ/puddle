// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decisions about which identity covers a request.

use super::*;

// Which identity.

#[tokio::test]
async fn a_request_no_identity_covers_goes_out_as_the_workspace_sent_it() {
    let w = World::new();
    w.identity("Work", "github.com", &["acme"], false, "CANARY-WORK");
    // The workspace may supply a token after the server's 401 (a credential helper, the
    // address's user name and password), so the request is neither given a credential nor
    // answered for.
    let decision = w
        .decide(
            "github.com",
            "GET",
            "/someone/private.git/info/refs?service=git-upload-pack",
            &[],
        )
        .await;
    assert!(
        matches!(decision, InjectDecision::PassThroughNoting(_)),
        "{decision:?}"
    );
    assert_eq!(w.credentials.reads(), 0);
    // A host no identity names at all.
    let other = w
        .decide(
            "gitlab.example",
            "GET",
            "/g/p.git/info/refs?service=git-upload-pack",
            &[],
        )
        .await;
    assert!(matches!(other, InjectDecision::PassThroughNoting(_)));
}

#[tokio::test]
async fn an_identity_is_picked_by_owner_and_changes_apply_at_the_next_request() {
    let w = world();
    let personal = w.store.identities(puddle_store::UserId::OWNER).unwrap()[1].id;
    assert!(injected(
        &w.decide("github.com", "GET", "/acme/x.git/info/refs", &[])
            .await,
        &token_header("CANARY-WORK")
    ));
    // Detaching Work: acme falls to the rest of the host, at once.
    let work = w.store.identities(puddle_store::UserId::OWNER).unwrap()[0].id;
    w.store.detach_identity(&w.workspace, work).unwrap();
    assert!(injected(
        &w.decide("github.com", "GET", "/acme/x.git/info/refs", &[])
            .await,
        &token_header("CANARY-PERSONAL")
    ));
    w.store.detach_identity(&w.workspace, personal).unwrap();
    assert!(matches!(
        w.decide("github.com", "GET", "/acme/x.git/info/refs", &[])
            .await,
        InjectDecision::PassThroughNoting(_)
    ));
}

#[tokio::test]
async fn two_candidates_are_refused_never_guessed() {
    let w = World::new();
    let a = w.identity_in_store("Work", "github.com", &["acme"], false, "CANARY-WORK", None);
    let b = w.identity_in_store(
        "Clash",
        "github.com",
        &["acme"],
        false,
        "CANARY-CLASH",
        None,
    );
    let mut git = WorkspaceGit::unconfigured();
    git.identities = vec![
        w.store.identity(puddle_store::UserId::OWNER, a.id).unwrap(),
        w.store.identity(puddle_store::UserId::OWNER, b.id).unwrap(),
    ];
    git.only_push_listed = false;
    let settings = StaticSettings::new(git);
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
    let decision = injector.decide(&context, &view).await;
    let (status, code, message) = refusal(&decision);
    assert_eq!((status, code), (409, "credential_ambiguous"));
    assert_eq!(
        message,
        "Work and Clash both cover github.com/acme; narrow one in Puddle"
    );
    assert_eq!(w.credentials.reads(), 0);
}
