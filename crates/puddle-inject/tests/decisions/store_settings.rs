// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decisions about settings and secrets read through the store and its cache.

use super::*;

/// A source that answers one token and counts its reads, behind the real cache.
struct OneToken(Arc<AtomicUsize>);

impl Fetch for OneToken {
    fn fetch(&self, _: &SourceSpec) -> impl Future<Output = Result<Fetched, SourceError>> + Send {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::ready(Ok(Fetched {
            credential: Credential {
                username: None,
                secret: Arc::new(Secret::new("CANARY-CACHED".to_owned())),
            },
            valid_for: None,
        }))
    }
}

/// The store's one write connection can be held for seconds (a long write, a backup), so asking
/// for the settings must not read the store on the spot, on the proxy's async thread: the read
/// runs when the answer is awaited, on a blocking thread.
#[tokio::test]
async fn the_store_settings_are_read_when_awaited_not_when_asked_for() {
    use puddle_inject::GitSettings as _;
    let w = World::new();
    let _work = w.identity("Work", "github.com", &["acme"], false, "unused");
    let settings = StoreSettings::new(Arc::clone(&w.store), w.workspace.clone());
    let asked = settings.current();
    puddle_store::testing::break_database(&w.store, "ALTER TABLE workspace_repos RENAME TO gone");
    assert!(asked.await.is_err(), "read before it was awaited");
}

#[tokio::test]
async fn the_secrets_cache_is_a_credential_source_that_reads_once_and_forgets_on_request() {
    let w = World::new();
    let made = w.identity("Work", "github.com", &["acme"], false, "unused");
    let reads = Arc::new(AtomicUsize::new(0));
    let cache = Arc::new(SecretCache::new(OneToken(Arc::clone(&reads))));
    let injector = GitInjector::new(
        w.workspace.clone(),
        Arc::new(StoreSettings::new(
            Arc::clone(&w.store),
            w.workspace.clone(),
        )),
        cache.clone(),
        w.events.clone(),
    );
    let host = Host::parse_normalised("github.com").unwrap();
    let context = InjectContext {
        workspace: &w.workspace,
        host: &host,
    };
    let lines = vec!["host: github.com".to_owned()];
    let view = RequestView::new("GET", "/acme/web.git/info/refs", &lines);
    for _ in 0..3 {
        let decision = injector.decide(&context, &view).await;
        assert!(
            injected(&decision, &token_header("CANARY-CACHED")),
            "{decision:?}"
        );
    }
    // Three requests, one read of the source; forgetting it makes the next request read again.
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    CredentialSource::forget(&*cache, &made.source);
    let _ = injector.decide(&context, &view).await;
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_workspace_that_never_set_a_switch_is_held_to_the_global_default_live() {
    let w = World::new();
    let _work = w.identity("Work", "github.com", &["acme"], false, "unused");
    let injector = GitInjector::new(
        w.workspace.clone(),
        Arc::new(StoreSettings::new(
            Arc::clone(&w.store),
            w.workspace.clone(),
        )),
        Arc::new(SecretCache::new(OneToken(Arc::new(AtomicUsize::new(0))))),
        w.events.clone(),
    );
    let host = Host::parse_normalised("github.com").unwrap();
    let context = InjectContext {
        workspace: &w.workspace,
        host: &host,
    };
    let lines = vec!["host: github.com".to_owned()];
    let push = RequestView::new("POST", "/acme/web.git/git-receive-pack", &lines);
    let fetch = RequestView::new("GET", "/acme/web.git/info/refs", &lines);

    // The built-in default: the push list on (an unlisted push is refused), the pull list off.
    assert_eq!(
        refusal(&injector.decide(&context, &push).await).1,
        "push_denied"
    );
    assert!(!matches!(
        injector.decide(&context, &fetch).await,
        InjectDecision::Refuse { .. }
    ));

    // The user turns the default's pull list on: the very next fetch is refused, no restart.
    w.store
        .set_git_defaults(puddle_store::UserId::OWNER, None, Some(true))
        .unwrap();
    assert_eq!(
        refusal(&injector.decide(&context, &fetch).await).1,
        "pull_denied"
    );
    // And the push list off: an unlisted push now passes.
    w.store
        .set_git_defaults(puddle_store::UserId::OWNER, Some(false), None)
        .unwrap();
    assert!(
        matches!(
            injector.decide(&context, &push).await,
            InjectDecision::PassThroughNoting(_) | InjectDecision::Inject { .. }
        ),
        "push list off by default"
    );
    // A switch the workspace sets itself wins over later changes of the default.
    w.store
        .set_git_switches(&w.workspace, Some(true), None)
        .unwrap();
    w.store
        .set_git_defaults(puddle_store::UserId::OWNER, Some(false), Some(false))
        .unwrap();
    assert_eq!(
        refusal(&injector.decide(&context, &push).await).1,
        "push_denied"
    );
}
