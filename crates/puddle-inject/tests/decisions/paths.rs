// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decisions about which paths are Git paths, how a path is read and why one is refused.

use super::*;

// On a bound git path: the workspace's header passes unchanged, a request with none gets the
// credential, a stand-in is a header the workspace sent.

#[tokio::test]
async fn on_a_git_path_a_request_without_authorization_gets_the_covering_credential() {
    let w = world();
    let work = w.decide("github.com", "GET", FETCH, &[]).await;
    assert!(injected(&work, &token_header("CANARY-WORK")), "{work:?}");
    let InjectDecision::Inject(injection) = &work else {
        unreachable!()
    };
    assert!(injection.binding_id().starts_with("identity:"));
    assert!(injection.binding_id().ends_with(":github.com"));
    // An owner no identity names is covered by the rest of the host.
    let other = w
        .decide("github.com", "GET", "/someone/else.git/info/refs", &[])
        .await;
    assert!(
        injected(&other, &token_header("CANARY-PERSONAL")),
        "{other:?}"
    );
    assert_eq!(w.credentials.reads(), 2);
}

#[tokio::test]
async fn on_a_git_path_the_workspaces_own_authorization_passes_unchanged() {
    let w = world();
    for header in [
        OWN,
        STAND_IN,
        "Authorization: x",
        "AUTHORIZATION: Basic Zm9v",
    ] {
        for target in [FETCH, PUSH_ADVERT, "/acme/web.git/git-upload-pack"] {
            let method = if target.ends_with("pack") {
                "POST"
            } else {
                "GET"
            };
            let decision = w.decide("github.com", method, target, &[header]).await;
            assert!(
                matches!(decision, InjectDecision::PassThroughOwnCredential),
                "{method} {target} with {header}: {decision:?}"
            );
        }
    }
    assert_eq!(w.credentials.reads(), 0, "no secret was even read");
}

// On a bound host's other paths: nothing is added, whatever the header says.

#[tokio::test]
async fn on_a_bound_host_a_path_that_is_not_git_is_never_given_a_credential() {
    let w = world();
    for target in [
        "/acme/web/archive/refs/heads/main.zip",
        "/acme/web",
        "/",
        "/login/oauth/access_token",
        "/acme/web/blob/main/info/refs",
    ] {
        for headers in [&[][..], &[OWN][..], &[STAND_IN][..]] {
            let decision = w.decide("github.com", "GET", target, headers).await;
            assert!(
                matches!(decision, InjectDecision::PassThrough),
                "{target} with {headers:?}: {decision:?}"
            );
        }
    }
    assert_eq!(w.credentials.reads(), 0);
}

// Path tricks.

#[tokio::test]
async fn a_path_that_names_a_repository_two_ways_is_refused_before_anything_else() {
    let w = world();
    for target in [
        "/acme/web.git/../other.git/git-receive-pack",
        "/acme%2Fother/web.git/git-receive-pack",
        "/acme//web.git/git-receive-pack",
        "/acme\\other/web.git/git-receive-pack",
        "/acme/web.git/git-receive-pack%00",
        "/acme/%77eb.git/git-receive-pack",
    ] {
        for headers in [&[][..], &[OWN][..]] {
            let decision = w.decide("github.com", "POST", target, headers).await;
            let (status, code, message) = refusal(&decision);
            assert_eq!((status, code), (400, "bad_git_path"), "{target}");
            assert!(
                message.contains("nothing was sent to github.com"),
                "{message}"
            );
        }
    }
    assert_eq!(w.credentials.reads(), 0);
}

// Why a path is refused.

#[tokio::test]
async fn a_refused_path_says_which_spelling_it_cannot_read() {
    let w = world();
    for (target, why) in [
        ("/acme/web.git/info\\refs", "a backslash"),
        (
            "/acme/web.git;x/info/refs?service=git-upload-pack",
            "a path parameter (;)",
        ),
        ("/acme/web.git/git-upload-pack\u{1}", "a control character"),
        ("/acme/web.git/git-upload-pack\u{7f}", "a control character"),
        (
            "/acme/we\u{e9}.git/git-upload-pack",
            "a non-ASCII character",
        ),
        (
            "/acme/we b.git/git-upload-pack",
            "a character that must be percent-encoded",
        ),
        ("/acme/web.git/./git-upload-pack", "a dot segment"),
        (
            "/acme//web.git/git-upload-pack",
            "an empty path segment (// or a trailing /)",
        ),
        (
            "/acme/web%2egit/git-upload-pack",
            "a percent-encoding that is not canonical",
        ),
    ] {
        let decision = w.decide("github.com", "POST", target, &[]).await;
        let (status, code, message) = refusal(&decision);
        assert_eq!((status, code), (400, "bad_git_path"), "{target}");
        assert!(message.contains(&format!("({why})")), "{target}: {message}");
    }
}

// Where a repository's path ends.

#[tokio::test]
async fn a_repository_nested_as_deep_as_the_host_allows_is_read_and_one_deeper_is_refused() {
    let w = World::new();
    w.identity("Lab", "gitlab.example", &["group"], false, "CANARY-LAB");
    w.switches(OnlyListed::Neither);
    let names = |count: usize| {
        (1..=count)
            .map(|n| format!("n{n}"))
            .collect::<Vec<_>>()
            .join("/")
    };
    let deepest = format!("/group/{}/git-upload-pack", names(21));
    let decision = w.decide("gitlab.example", "POST", &deepest, &[]).await;
    assert!(
        injected(&decision, &token_header("CANARY-LAB")),
        "{decision:?}"
    );
    let too_deep = format!("/group/{}/git-upload-pack", names(22));
    let decision = w.decide("gitlab.example", "POST", &too_deep, &[]).await;
    let (status, code, message) = refusal(&decision);
    assert_eq!((status, code), (400, "bad_git_path"));
    assert!(
        message.contains("deeper than any Git host has"),
        "{message}"
    );
}
