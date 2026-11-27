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

/// The owners table: never more than [`MAX_NOTICES`] keys, the newest always kept, stale ones
/// dropped first.
#[tokio::test(start_paused = true)]
async fn the_owners_table_is_bounded_and_keeps_the_newest() {
    let size = |t: &Notices| t.last.lock().unwrap().len();
    let full = || {
        let table = Notices::default();
        for i in 0..MAX_NOTICES {
            table.mark(&format!("owner{i}"));
        }
        table
    };
    let start = Instant::now();
    let table = full();
    assert_eq!(size(&table), MAX_NOTICES, "a full table has lost nothing");
    // All recent: another key makes room for the newest, which stays.
    table.mark("newest");
    assert_eq!(size(&table), MAX_NOTICES);
    assert!(table.marked_since("newest", start));
    // A key the table holds again makes no room.
    table.mark("newest");
    assert_eq!(size(&table), MAX_NOTICES);

    // Just short of the ten seconds the table keeps keys, none is stale yet.
    let table = full();
    tokio::time::advance(NOTICE_EVERY.saturating_sub(Duration::from_millis(1))).await;
    table.mark("later");
    assert_eq!(size(&table), MAX_NOTICES);
    assert!(table.marked_since("later", start));
    // A table that is not past its size is not swept, stale keys and all.
    let table = full();
    tokio::time::advance(NOTICE_EVERY).await;
    table.mark("owner0");
    assert_eq!(size(&table), MAX_NOTICES);
    // Past its size, at ten seconds every older key is stale and goes.
    let table = full();
    tokio::time::advance(NOTICE_EVERY).await;
    table.mark("later");
    assert_eq!(size(&table), 1);
}

#[test]
fn a_batch_body_that_is_no_object_says_what_was_expected() {
    let err = serde::Deserializer::deserialize_map(
        &mut serde_json::Deserializer::from_slice(b"[]"),
        OnlyOperation,
    )
    .unwrap_err();
    assert!(err.to_string().contains("an LFS batch object"), "{err}");
}

/// What the injector decides for a GET of `target` on `host`.
async fn decided(w: &crate::testing::World, host: &str, target: &str) -> InjectDecision {
    let host = puddle_types::Host::parse_normalised(host).unwrap();
    let context = InjectContext {
        workspace: &w.workspace,
        host: &host,
    };
    let lines = vec![format!("host: {host}")];
    let view = RequestView::new("GET", target, &lines);
    Injector::decide(&*w.injector, &context, &view).await
}

/// `Basic` with the user `x-access-token` and `token`, as the header carries it.
fn basic_header(token: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"))
    )
}

/// A token for one Azure DevOps organisation goes only to that organisation, however the
/// organisation is spelled, and a row that covers another one is refused before anything is read.
#[tokio::test]
async fn a_token_for_one_organisation_goes_only_to_that_organisation() {
    let w = crate::testing::World::new();
    // Covers `Contoso` (the guest's spelling of the path does not matter), token for `CONTOSO`.
    let right = w.identity_in_store(
        "Right",
        "dev.azure.com",
        &["contoso"],
        false,
        "CANARY-RIGHT",
        Some("CONTOSO"),
    );
    w.store
        .attach_identity(&w.workspace, right.id, None)
        .unwrap();
    // The store refuses an identity whose token is for another organisation than it covers, so a
    // hand-edited row stands in: covers `fabrikam`, its token belongs to `contoso`.
    let wrong = w.identity_in_store(
        "Wrong",
        "dev.azure.com",
        &["fabrikam"],
        false,
        "CANARY-WRONG",
        None,
    );
    let mut identity = w
        .store
        .identity(puddle_store::UserId::OWNER, wrong.id)
        .unwrap();
    let mut json = serde_json::to_value(&identity.credentials).unwrap();
    json[0]["source"] = serde_json::json!({
        "kind": "stored", "id": "t-wrong", "scope": {"host": "dev.azure.com", "org": "contoso"}
    });
    identity.credentials = serde_json::from_value(json).unwrap();
    let mut git = WorkspaceGit::unconfigured();
    git.identities = vec![identity];
    git.only_push_listed = false;
    let hand_edited = GitInjector::new(
        w.workspace.clone(),
        crate::testing::StaticSettings::new(git),
        w.credentials.clone(),
        w.events.clone(),
    );
    let host = puddle_types::Host::parse_normalised("dev.azure.com").unwrap();
    let context = InjectContext {
        workspace: &w.workspace,
        host: &host,
    };
    let lines = vec!["host: dev.azure.com".to_owned()];
    let view = RequestView::new("GET", "/fabrikam/p/_git/r/info/refs", &lines);
    let refused = Injector::decide(&hand_edited, &context, &view).await;
    let InjectDecision::Refuse(refusal) = refused else {
        panic!("a token for another organisation went out: {refused:?}");
    };
    assert_eq!(refusal.status(), 502);
    assert_eq!(refusal.code(), ReasonCode::CredentialUnavailable);
    assert!(
        refusal
            .message()
            .contains("is not for dev.azure.com/fabrikam"),
        "{}",
        refusal.message()
    );
    assert_eq!(
        w.credentials.reads(),
        0,
        "the token was read for another organisation"
    );

    for target in ["/contoso/p/_git/r/info/refs", "/Contoso/p/_git/r/info/refs"] {
        let sent = decided(&w, "dev.azure.com", target).await;
        let InjectDecision::Inject(injection) = sent else {
            panic!("{target}: {sent:?}");
        };
        assert_eq!(injection.headers().len(), 1, "{target}");
        assert!(
            injection.headers()[0].value_is(&format!("Basic {}", {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(":CANARY-RIGHT")
            })),
            "{target}"
        );
    }
}

/// The `401` of the real server becomes a `403` that names the identity and the source, forgets
/// the cached secret and raises the sign-in notice once.
#[tokio::test]
async fn a_401_becomes_a_403_that_forgets_the_secret_and_raises_one_notice() {
    let w = crate::testing::World::new();
    let made = w.identity("Work", "github.com", &["acme"], false, "CANARY-WORK");
    let identity = w.store.identities(puddle_store::UserId::OWNER).unwrap()[0].clone();
    let Classified::Git(path) = classify("github.com", "GET", "/acme/web.git/info/refs", None)
    else {
        panic!("a Git path");
    };
    let rejected = w.injector.rejected(&identity, &made.source, &path);
    assert!(
        w.credentials.forgotten().is_empty(),
        "forgotten before the 401"
    );

    let refusal = rejected.refusal().expect("a refusal replaces the 401");
    assert_eq!(refusal.status(), 403);
    assert_eq!(refusal.code(), ReasonCode::CredentialRejected);
    assert!(
        refusal.message().contains("identity Work"),
        "{}",
        refusal.message()
    );
    assert!(
        refusal.message().contains("github.com/acme"),
        "{}",
        refusal.message()
    );
    assert!(
        refusal.message().contains(&made.source.describe()),
        "{}",
        refusal.message()
    );
    assert!(!refusal.message().contains("CANARY"));
    assert_eq!(
        w.credentials.forgotten(),
        std::slice::from_ref(&made.source)
    );
    let notices = |w: &crate::testing::World| {
        w.events
            .take()
            .iter()
            .filter(|e| matches!(e, Event::CredentialSignInNeeded { .. }))
            .count()
    };
    assert_eq!(notices(&w), 1);

    // A second `401` forgets again (the user may have signed in again) but does not repeat the notice.
    let again = rejected.refusal().expect("the same refusal again");
    assert_eq!(
        (again.status(), again.code()),
        (403, ReasonCode::CredentialRejected)
    );
    assert_eq!(w.credentials.forgotten().len(), 2);
    assert_eq!(notices(&w), 0);
}

/// The token leaves as `Basic base64(user:token)`, so the plain-text canary search cannot see it:
/// the header carries it, and neither the decision's `Debug` text nor its base64 form shows it.
#[tokio::test]
async fn the_token_is_in_the_header_and_in_nothing_that_prints() {
    let w = crate::testing::World::new();
    w.identity("Work", "github.com", &["acme"], false, "CANARY-WORK");
    w.identity("Personal", "github.com", &[], true, "CANARY-PERSONAL");
    let decision = decided(&w, "github.com", "/acme/web.git/info/refs").await;
    let shown = format!("{decision:?}");
    let InjectDecision::Inject(injection) = &decision else {
        panic!("{decision:?}");
    };
    assert!(injection.headers()[0].value_is(&basic_header("CANARY-WORK")));
    assert!(!injection.headers()[0].value_is(&basic_header("CANARY-PERSONAL")));
    let encoded = basic_header("CANARY-WORK");
    let base64 = encoded.trim_start_matches("Basic ");
    for (name, secret) in [
        ("token", "CANARY-WORK"),
        ("base64", base64),
        ("header", encoded.as_str()),
    ] {
        assert!(
            !shown.contains(secret),
            "the {name} is printed in the decision"
        );
    }
    assert!(shown.contains("<redacted>"), "{shown}");
}
