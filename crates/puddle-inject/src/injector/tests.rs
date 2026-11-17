// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn rejected() -> Unauthorized {
    Unauthorized::noting(|| {})
}

#[test]
fn a_value_that_cannot_be_a_header_is_refused_as_an_unavailable_credential() {
    let refused = credentialed(
        SecretValue::new("Basic a\nb"),
        "identity:1:github.com".to_owned(),
        "Work",
        "github.com/acme",
        rejected(),
    );
    let InjectDecision::Refuse(refusal) = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(refusal.status(), 502);
    assert_eq!(refusal.code(), ReasonCode::CredentialUnavailable);
    assert!(refusal.message().contains("identity Work"));
    assert!(refusal.message().contains("github.com/acme"));
}

#[test]
fn a_plain_value_becomes_the_authorization_header() {
    let decision = credentialed(
        SecretValue::new("Basic YTpi"),
        "identity:1:github.com".to_owned(),
        "Work",
        "github.com/acme",
        rejected(),
    );
    let InjectDecision::Inject(injection) = decision else {
        panic!("{decision:?}");
    };
    assert_eq!(injection.binding_id(), "identity:1:github.com");
}

/// Refused pushes to repositories the guest names at will, enough to fill a table of notices.
async fn flood_of_refused_pushes(w: &crate::testing::World) {
    let host = puddle_types::Host::parse_normalised("github.com").unwrap();
    let context = InjectContext {
        workspace: &w.workspace,
        host: &host,
    };
    let lines = vec!["host: github.com".to_owned()];
    for i in 0..(2 * MAX_NOTICES) {
        let target = format!("/acme/made-up-{i}.git/info/refs?service=git-receive-pack");
        let view = RequestView::new("GET", &target, &lines);
        let decision = Injector::decide(&*w.injector, &context, &view).await;
        assert!(
            matches!(decision, InjectDecision::Refuse(_)),
            "{decision:?}"
        );
    }
}

#[tokio::test]
async fn a_flood_of_refused_pushes_does_not_silence_the_sign_in_notice() {
    let w = crate::testing::World::new();
    let made = w.identity("Work", "github.com", &["acme"], false, "CANARY-WORK");
    w.switches(crate::testing::OnlyListed::Push);
    flood_of_refused_pushes(&w).await;
    let _ = w.events.take();

    let identity = w.store.identities(puddle_store::UserId::OWNER).unwrap()[0].clone();
    let Classified::Git(path) = classify("github.com", "GET", "/acme/web.git/info/refs", None)
    else {
        panic!("a Git path");
    };
    let refusal = w
        .injector
        .rejected(&identity, &made.source, &path)
        .refusal();
    assert!(refusal.is_some());
    assert!(
        w.events
            .take()
            .iter()
            .any(|e| matches!(e, Event::CredentialSignInNeeded { .. })),
        "the sign-in notice was silenced"
    );
}

#[tokio::test]
async fn a_flood_of_refused_pushes_does_not_silence_the_no_identity_notice() {
    tokio::time::pause();
    let w = crate::testing::World::new();
    w.switches(crate::testing::OnlyListed::Push);
    flood_of_refused_pushes(&w).await;
    let _ = w.events.take();

    let Classified::Git(path) =
        classify("github.com", "GET", "/someone/private.git/info/refs", None)
    else {
        panic!("a Git path");
    };
    assert!(w.injector.unanswered(&path).refusal().is_none());
    tokio::task::yield_now().await;
    tokio::time::advance(NO_IDENTITY_GRACE + Duration::from_millis(500)).await;
    tokio::task::yield_now().await;
    assert!(
        w.events
            .take()
            .iter()
            .any(|e| matches!(e, Event::GitNoIdentity { .. })),
        "the no-identity notice was silenced"
    );
}
