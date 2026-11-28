// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// A source that always needs the user to sign in.
struct NotSignedIn(Mutex<usize>);
impl Fetch for NotSignedIn {
    fn fetch(&self, _: &SourceSpec) -> impl Future<Output = Result<Fetched, SourceError>> + Send {
        *self.0.lock().unwrap() += 1;
        std::future::ready(Err(SourceError::NotSignedIn))
    }
}
/// A source that answers from a script and has a value worth leaking.
struct Scripted(Mutex<Vec<Result<(), SourceError>>>);
impl Fetch for Scripted {
    fn fetch(&self, _: &SourceSpec) -> impl Future<Output = Result<Fetched, SourceError>> + Send {
        let next = self.0.lock().unwrap().remove(0);
        std::future::ready(next.map(|()| Fetched {
            credential: puddle_secrets::Credential {
                username: None,
                secret: Arc::new(Secret::new("hunter2-the-value".into())),
            },
            valid_for: None,
        }))
    }
}
#[tokio::test]
async fn every_use_updates_what_the_last_read_came_to_and_the_value_is_never_kept() {
    use puddle_secrets::ReadUse;
    let events = Arc::new(Events::default());
    let reads = credential_read_log(events.clone());
    let secrets = SecretCache::new(Scripted(Mutex::new(vec![
        Ok(()),
        Err(SourceError::NotSignedIn),
        Err(SourceError::NotSignedIn),
    ])))
    .with_read_log(Arc::clone(&reads));
    let spec = SourceSpec::Gh {
        host: HostName::new("github.com").unwrap(),
        account: AccountName::new("me").unwrap(),
    };
    assert_eq!(reads.last(&spec), None, "no read yet");

    secrets.get(&spec).await.unwrap();
    let first = reads.last(&spec).unwrap();
    assert_eq!(
        (first.by, first.problem.clone()),
        (ReadUse::Injection, None)
    );
    assert_eq!(told(&events), [Event::IdentitiesChanged {}]);
    assert!(!format!("{first:?}").contains("hunter2"));

    // A repository list forces a new read (the entry is invalidated) that fails.
    secrets.invalidate(&spec);
    assert!(
        secrets
            .get_for(&spec, ReadUse::RepositoryList)
            .await
            .is_err()
    );
    let second = reads.last(&spec).unwrap();
    assert_eq!(
        (second.by, second.problem.clone()),
        (ReadUse::RepositoryList, Some(SourceError::NotSignedIn))
    );
    assert_eq!(told(&events).len(), 2, "readable to a problem is news");

    // The same failure again moves nothing, but is still the latest read.
    assert!(secrets.get_for(&spec, ReadUse::Injection).await.is_err());
    assert_eq!(reads.last(&spec).unwrap().by, ReadUse::Injection);
    assert_eq!(told(&events).len(), 2);
}
#[tokio::test]
async fn a_source_that_needs_a_sign_in_becomes_one_notice_naming_it() {
    let secrets = Arc::new(SecretCache::new(NotSignedIn(Mutex::new(0))));
    let events = Arc::new(Events::default());
    let task = forward_sign_in_needed(&secrets, events.clone());
    let host = HostName::new("github.com").unwrap();
    let spec = SourceSpec::Gh {
        host,
        account: AccountName::new("me").unwrap(),
    };
    assert!(secrets.get(&spec).await.is_err());
    puddle_test_kit::eventually("a sign-in notice forwarded", Duration::from_secs(2), || {
        !events.0.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        *events.0.lock().unwrap(),
        [Event::CredentialSignInNeeded {
            host: "github.com".into(),
            source: "gh account me on github.com".into(),
        }]
    );
    // The forwarder ends with the cache it listens to.
    drop(secrets);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test(start_paused = true)]
async fn a_retrying_client_is_one_sign_in_notice_per_source_per_interval() {
    let secrets = Arc::new(SecretCache::new(NotSignedIn(Mutex::new(0))));
    let events = Arc::new(Events::default());
    let task = forward_sign_in_needed(&secrets, events.clone());
    let spec = |host: &str| SourceSpec::Gh {
        host: HostName::new(host).unwrap(),
        account: AccountName::new("me").unwrap(),
    };
    for _ in 0..5 {
        assert!(secrets.get(&spec("github.com")).await.is_err());
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        events.0.lock().unwrap().len(),
        1,
        "five failed reads, one notice"
    );
    // Another source is its own notice.
    assert!(secrets.get(&spec("ghe.example")).await.is_err());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(events.0.lock().unwrap().len(), 2);
    // Once the interval is over the same source is noticed again.
    tokio::time::sleep(SIGN_IN_NOTICE_EVERY).await;
    assert!(secrets.get(&spec("github.com")).await.is_err());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(events.0.lock().unwrap().len(), 3);
    task.abort();
}
#[tokio::test(start_paused = true)]
async fn more_sources_than_the_cap_in_one_interval_are_not_all_noticed() {
    let secrets = Arc::new(SecretCache::new(NotSignedIn(Mutex::new(0))));
    let events = Arc::new(Events::default());
    let task = forward_sign_in_needed(&secrets, events.clone());
    let total = MAX_SIGN_IN_SOURCES + 10;
    // flake-ok: paced flood on the paused clock, one source per tick
    for i in 0..total {
        let spec = SourceSpec::Gh {
            host: HostName::new(format!("host{i}.example")).unwrap(),
            account: AccountName::new("me").unwrap(),
        };
        assert!(secrets.get(&spec).await.is_err());
        // Let the forwarder keep up: the cache's buffer is smaller than the flood.
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(events.0.lock().unwrap().len(), MAX_SIGN_IN_SOURCES);
    task.abort();
}
#[tokio::test]
async fn notices_over_the_buffer_lose_the_oldest_and_the_rest_still_arrive() {
    let secrets = Arc::new(SecretCache::new(NotSignedIn(Mutex::new(0))));
    let events = Arc::new(Events::default());
    // The forwarder cannot run until this test awaits something that is not ready, so the
    // notices pile up past the cache's buffer (16) and it must skip what was lost.
    let task = forward_sign_in_needed(&secrets, events.clone());
    for i in 0..40 {
        let host = HostName::new(format!("host{i}.example")).unwrap();
        let spec = SourceSpec::Gh {
            host,
            account: AccountName::new("me").unwrap(),
        };
        assert!(secrets.get(&spec).await.is_err());
    }
    puddle_test_kit::eventually("16 notices forwarded", Duration::from_secs(2), || {
        events.0.lock().unwrap().len() >= 16
    })
    .await;
    let seen = events.0.lock().unwrap().len();
    assert!((1..40).contains(&seen), "{seen} notices");
    task.abort();
}
#[test]
fn the_inputs_print_without_what_they_hold() {
    let text = format!("{:?}", inputs(&store()));
    assert_eq!(text, "InjectorInputs { .. }");
}
#[tokio::test]
async fn a_git_read_whose_source_needs_a_sign_in_goes_out_anonymously_and_the_user_gets_one_notice()
{
    // The shape of the default injector, over a cache whose source is never signed in: the
    // request goes out anonymously, and the one notice is the cache's, forwarded.
    let store = store();
    let ws = workspace("alpha");
    attach(&store, &ws, "ada", "github.com");
    let secrets = Arc::new(SecretCache::new(NotSignedIn(Mutex::new(0))));
    let events = Arc::new(Events::default());
    let task = forward_sign_in_needed(&secrets, events.clone());
    let credentials: Arc<dyn CredentialSource> = secrets.clone();
    let injector = GitInjector::new(
        ws.clone(),
        Arc::new(StoreSettings::new(store, ws.clone())),
        credentials,
        events.clone(),
    );
    let host = Host::parse_normalised("github.com").unwrap();
    let context = InjectContext {
        workspace: &ws,
        host: &host,
    };
    let lines = ["host: github.com".to_owned()];
    let view = RequestView::new(
        "GET",
        "/acme/web.git/info/refs?service=git-upload-pack",
        &lines,
    );
    // A read goes out with no credential; a private repository's `401` is the sign-in line.
    let decision = injector.decide(&context, &view).await;
    assert!(
        format!("{decision:?}").starts_with("PassThroughNoting"),
        "{decision:?}"
    );
    puddle_test_kit::eventually("a sign-in notice forwarded", Duration::from_secs(2), || {
        !events.0.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        *events.0.lock().unwrap(),
        [Event::CredentialSignInNeeded {
            host: "github.com".into(),
            source: "gh account me on github.com".into(),
        }]
    );
    // The forwarder ends with the cache it listens to; the injector holds a handle too.
    drop(secrets);
    drop(injector);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}
