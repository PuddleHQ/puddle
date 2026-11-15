// SPDX-License-Identifier: AGPL-3.0-or-later
use tokio::sync::broadcast::error::TryRecvError;

use super::*;

fn proxy() -> ProxyAddr {
    ProxyAddr::new("proxy.corp", 8080)
}

/// The next notice `record` has sent.
// flake-ok: `record` sends on the broadcast channel before it returns, on the caller's thread (these
// are plain `#[test]`s with no task), so the channel already holds every notice there is.
fn received(failures: &mut broadcast::Receiver<SignIn>) -> Result<SignIn, TryRecvError> {
    failures.try_recv()
}

#[test]
fn a_failure_is_announced_once_per_episode_and_never_with_a_secret() {
    let log = SignInLog::default();
    let mut failures = log.failures();
    let record = |outcome, detail: Option<&str>| {
        log.record(
            &proxy(),
            Some("Basic".into()),
            outcome,
            detail.map(str::to_owned),
        );
    };
    record(SignInOutcome::SignedIn, None);
    assert_eq!(received(&mut failures).unwrap_err(), TryRecvError::Empty);

    record(SignInOutcome::Failed, Some("rejected Basic abc123=="));
    let notice = received(&mut failures).unwrap();
    assert_eq!(notice.proxy, proxy());
    assert_eq!(notice.scheme.as_deref(), Some("Basic"));
    assert_eq!(notice.outcome, SignInOutcome::Failed);
    assert!(
        !format!("{notice:?}").contains("abc123"),
        "the notice is cleaned like the report: {notice:?}"
    );

    // A client retrying the same wrong password is the same episode.
    record(SignInOutcome::Failed, None);
    assert_eq!(received(&mut failures).unwrap_err(), TryRecvError::Empty);
    // Unsupported is not a failed attempt: nothing to correct by retrying.
    record(SignInOutcome::Unsupported, None);
    assert_eq!(received(&mut failures).unwrap_err(), TryRecvError::Empty);
    // After it worked, the next failure is news again; another proxy has its own episode.
    record(SignInOutcome::SignedIn, None);
    record(SignInOutcome::Failed, None);
    assert!(received(&mut failures).is_ok());
    log.record(
        &ProxyAddr::new("other.corp", 3128),
        None,
        SignInOutcome::Failed,
        None,
    );
    assert_eq!(received(&mut failures).unwrap().scheme, None);
}

#[test]
fn with_nobody_listening_a_failure_is_still_in_the_report() {
    let log = SignInLog::default();
    log.record(&proxy(), None, SignInOutcome::Failed, Some("no".into()));
    assert_eq!(log.all().len(), 1);
}
