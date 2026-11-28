// SPDX-License-Identifier: AGPL-3.0-or-later
//! A credential store that does not answer, answers late or refuses part of an answer's tokens.

use super::*;
use crate::test_rig::*;

/// A credential store that does not answer until it is let go.
struct Stuck(Mutex<std::sync::mpsc::Receiver<()>>);

impl SecretStore for Stuck {
    fn get(
        &self,
        _: &puddle_secrets::StoredId,
    ) -> Result<Option<puddle_secrets::Secret>, puddle_secrets::StoreError> {
        let _ = self.0.lock().unwrap().recv();
        Err(puddle_secrets::StoreError)
    }

    fn set(
        &self,
        _: &puddle_secrets::StoredId,
        _: &puddle_secrets::Secret,
    ) -> Result<(), puddle_secrets::StoreError> {
        Err(puddle_secrets::StoreError)
    }

    fn delete(&self, _: &puddle_secrets::StoredId) -> Result<(), puddle_secrets::StoreError> {
        Ok(())
    }
}

#[tokio::test]
async fn a_credential_store_that_does_not_answer_is_given_up_on_and_the_start_goes_on() {
    let (release, wait) = std::sync::mpsc::channel();
    let store: Arc<dyn SecretStore> = Arc::new(Stuck(Mutex::new(wait)));
    let notices = Arc::new(Notices::default());
    let logins = Logins::new(
        workspace(),
        true,
        builtin(),
        Arc::new(StandIns::new()),
        store,
        notices.clone(),
        Marker::none(),
    );
    logins.set_store_timeout(Duration::from_millis(50));
    logins.load().await;
    assert!(
        notices
            .seen()
            .contains(&("claude".to_owned(), Problem::StoreUnavailable))
    );
    // Let the blocked threads go, one for each slot that was waiting on the store.
    for _ in 0..8 {
        let _ = release.send(());
    }
}

/// What a [`Scripted`] store does at a `set`, before it stores the token.
enum SetStep {
    /// Says it has started (the sender) and waits until it is let go (the receiver).
    Park(
        tokio::sync::oneshot::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    ),
    /// Stores the token.
    Pass,
    /// Stores nothing and fails.
    Fail,
}

/// A credential store that keeps tokens like [`MemoryStore`] and does the next step of its script
/// at each `set`; with the script done, `set` just stores.
struct Scripted {
    inner: MemoryStore,
    sets: Mutex<std::collections::VecDeque<SetStep>>,
}

impl Scripted {
    fn new(steps: impl IntoIterator<Item = SetStep>) -> Self {
        Self {
            inner: MemoryStore::new(),
            sets: Mutex::new(steps.into_iter().collect()),
        }
    }

    /// Adds `steps` to the script.
    fn then(&self, steps: impl IntoIterator<Item = SetStep>) {
        self.sets.lock().unwrap().extend(steps);
    }
}

impl SecretStore for Scripted {
    fn get(
        &self,
        id: &puddle_secrets::StoredId,
    ) -> Result<Option<puddle_secrets::Secret>, puddle_secrets::StoreError> {
        self.inner.get(id)
    }

    fn set(
        &self,
        id: &puddle_secrets::StoredId,
        secret: &puddle_secrets::Secret,
    ) -> Result<(), puddle_secrets::StoreError> {
        let step = self.sets.lock().unwrap().pop_front();
        match step {
            Some(SetStep::Park(started, go)) => {
                let _ = started.send(());
                let _ = go.recv();
            }
            Some(SetStep::Fail) => return Err(puddle_secrets::StoreError),
            Some(SetStep::Pass) | None => {}
        }
        self.inner.set(id, secret)
    }

    fn delete(&self, id: &puddle_secrets::StoredId) -> Result<(), puddle_secrets::StoreError> {
        self.inner.delete(id)
    }
}

fn logins_on(store: &Arc<Scripted>, registry: &Arc<StandIns>) -> Logins {
    Logins::new(
        workspace(),
        true,
        builtin(),
        Arc::clone(registry),
        store.clone(),
        Arc::new(Notices::default()),
        Marker::none(),
    )
}

/// A Claude token answer as `logins` gives it to the tool, or `None` when the guest gets the
/// refusal for a sign-in that was not kept (never when the real answer reaches it).
async fn capture_claude(logins: &Logins, access: &str, refresh: &str) -> Option<Value> {
    let exchange = logins.begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    let answer = claude_answer(access, refresh);
    given_or_refused(rewritten(exchange, 200, Some("application/json"), &answer).await)
}

#[tokio::test]
async fn a_capture_that_cannot_store_every_token_leaves_the_earlier_pair_in_the_store() {
    let store = Arc::new(Scripted::new([]));
    let registry = Arc::new(StandIns::new());
    let logins = logins_on(&store, &registry);
    let first = capture_claude(&logins, REAL_ACCESS, REAL_REFRESH)
        .await
        .unwrap();
    // The refresh's new access token is stored, its new refresh token is not.
    store.then([SetStep::Pass, SetStep::Fail]);
    assert_eq!(capture_claude(&logins, ACCESS_B, REFRESH_B).await, None);
    let vault = Vault::new(store.clone(), &workspace());
    for (role, real, given) in [
        (Role::Access, REAL_ACCESS, &first["access_token"]),
        (Role::Refresh, REAL_REFRESH, &first["refresh_token"]),
    ] {
        let kept = vault.read(&CLAUDE, role).unwrap().unwrap();
        assert_eq!(
            (kept.stand_in.as_str(), kept.real.as_str()),
            (given.as_str().unwrap(), real),
            "the store holds a pair that never went together"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_token_the_store_writes_after_it_timed_out_does_not_outlast_the_put_back() {
    let store = Arc::new(Scripted::new([]));
    let registry = Arc::new(StandIns::new());
    let logins = logins_on(&store, &registry);
    let first = capture_claude(&logins, REAL_ACCESS, REAL_REFRESH)
        .await
        .unwrap();
    let limit = Duration::from_millis(100);
    logins.set_store_timeout(limit);
    // The refresh's new refresh token is written late: after the capture gave up on it.
    let (started, parked) = tokio::sync::oneshot::channel();
    let (go, wait) = std::sync::mpsc::channel();
    store.then([SetStep::Pass, SetStep::Park(started, wait)]);
    let exchange = logins.begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    let answer = claude_answer(ACCESS_B, REFRESH_B);
    let mut capture = Box::pin(rewritten(exchange, 200, Some("application/json"), &answer));
    // A parked blocking call stops the paused clock from advancing by itself: move it by hand,
    // once the write is parked, by one store timeout and no more.
    tokio::select! {
        _ = &mut capture => panic!("the capture ended with its write parked"),
        parked = parked => parked.unwrap(),
    }
    tokio::time::advance(limit + Duration::from_millis(1)).await;
    // The tool is answered while the late write is still parked, not after it.
    let given = tokio::select! {
        given = &mut capture => Some(given),
        () = async { for _ in 0..200 { tokio::task::yield_now().await } } => None,
    };
    assert_refused(
        &given.expect("the tool waited for the late write instead of being answered first"),
    );
    // The put back task starts its own wait on the store.
    // flake-ok: lets the spawned task run until it waits on the store gate; there is no condition to poll
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    // The write lands 300 ms after it started, long after the capture gave up on it; the put
    // back runs behind it. The clock moves on only once the blocking threads are done, so the
    // sleep below is the whole window.
    tokio::time::advance(Duration::from_millis(200)).await;
    let _ = go.send(());
    tokio::time::sleep(Duration::from_millis(700)).await;
    let vault = Vault::new(store.clone(), &workspace());
    for (role, real, given) in [
        (Role::Access, REAL_ACCESS, &first["access_token"]),
        (Role::Refresh, REAL_REFRESH, &first["refresh_token"]),
    ] {
        let kept = vault.read(&CLAUDE, role).unwrap().unwrap();
        assert_eq!(
            (kept.stand_in.as_str(), kept.real.as_str()),
            (given.as_str().unwrap(), real),
            "a write that finished after the put back left a pair that never went together"
        );
    }
}

/// A Claude token answer with an access token only: one slot, so a failed write has nothing to put back.
fn claude_access_only_answer(access: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "token_type": "Bearer",
        "access_token": access,
        "expires_in": 28800,
        "scope": "user:inference user:profile",
        "account": {"uuid": "u-1", "email_address": "me@example.invalid"},
    }))
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_write_that_times_out_with_nothing_to_put_back_refuses_after_one_timeout() {
    let store = Arc::new(Scripted::new([]));
    let registry = Arc::new(StandIns::new());
    let logins = logins_on(&store, &registry);
    let limit = Duration::from_secs(10);
    logins.set_store_timeout(limit);
    let (started, parked) = tokio::sync::oneshot::channel();
    let (go, wait) = std::sync::mpsc::channel();
    store.then([SetStep::Park(started, wait)]);
    let exchange = logins.begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    let answer = claude_access_only_answer(REAL_ACCESS);
    let mut capture = Box::pin(rewritten(exchange, 200, Some("application/json"), &answer));
    // A parked blocking call stops the paused clock from advancing by itself: move it by hand,
    // once the write is parked, by one store timeout and no more.
    tokio::select! {
        _ = &mut capture => panic!("the capture ended with its write parked"),
        parked = parked => parked.unwrap(),
    }
    tokio::time::advance(limit + Duration::from_millis(1)).await;
    let given = tokio::select! {
        given = &mut capture => Some(given),
        () = async { for _ in 0..200 { tokio::task::yield_now().await } } => None,
    };
    // Let the parked write go, so the blocking thread ends with the test.
    let _ = go.send(());
    let given = given.expect(
        "the capture lock was still held a store timeout after a write that left nothing to put back",
    );
    assert_refused(&given);
}

#[tokio::test]
async fn a_first_login_that_cannot_store_every_token_leaves_nothing_in_the_store() {
    let store = Arc::new(Scripted::new([SetStep::Pass, SetStep::Fail]));
    let registry = Arc::new(StandIns::new());
    let logins = logins_on(&store, &registry);
    assert_eq!(
        capture_claude(&logins, REAL_ACCESS, REAL_REFRESH).await,
        None
    );
    assert_eq!(store.inner.ids(), Vec::<String>::new());
    assert_eq!(registry.len(), 0);
}

#[tokio::test]
async fn an_entry_that_cannot_be_read_is_not_put_back_and_one_the_store_refuses_stays() {
    let store = Arc::new(Scripted::new([]));
    let registry = Arc::new(StandIns::new());
    let logins = logins_on(&store, &registry);
    capture_claude(&logins, REAL_ACCESS, REAL_REFRESH)
        .await
        .unwrap();
    let access = crate::vault::slot_id(workspace().as_str(), "claude", Role::Access).unwrap();
    store
        .inner
        .set(
            &access,
            &puddle_secrets::Secret::new("not an entry".to_owned()),
        )
        .unwrap();
    // The damaged access entry is not put back: it goes, and the refresh entry stays as it was.
    store.then([SetStep::Pass, SetStep::Fail]);
    assert_eq!(capture_claude(&logins, ACCESS_B, REFRESH_B).await, None);
    let vault = Vault::new(store.clone(), &workspace());
    assert!(vault.read(&CLAUDE, Role::Access).unwrap().is_none());
    let refresh = vault.read(&CLAUDE, Role::Refresh).unwrap().unwrap();
    assert_eq!(refresh.real.as_str(), REAL_REFRESH);
    // A store that refuses the put back too keeps what it was given: nothing more can be done.
    capture_claude(&logins, REAL_ACCESS, REAL_REFRESH)
        .await
        .unwrap();
    store.then([SetStep::Pass, SetStep::Fail, SetStep::Fail]);
    assert_eq!(capture_claude(&logins, ACCESS_B, REFRESH_B).await, None);
    let kept = vault.read(&CLAUDE, Role::Access).unwrap().unwrap();
    assert_eq!(kept.real.as_str(), ACCESS_B);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_login_forgotten_while_it_is_being_captured_stays_forgotten() {
    let (started, parked) = tokio::sync::oneshot::channel();
    let (go, wait) = std::sync::mpsc::channel();
    let store = Arc::new(Scripted::new([SetStep::Park(started, wait)]));
    let registry = Arc::new(StandIns::new());
    let logins = logins_on(&store, &registry);
    let capture = tokio::spawn({
        let logins = logins.clone();
        async move { capture_claude(&logins, REAL_ACCESS, REAL_REFRESH).await }
    });
    // The capture is writing its first token when the user forgets the login.
    parked.await.unwrap();
    let mut forget = tokio::spawn({
        let logins = logins.clone();
        async move { logins.forget("claude").await }
    });
    // A forget that does not wait for the capture is over well within this.
    let early = tokio::time::timeout(Duration::from_millis(200), &mut forget).await;
    go.send(()).unwrap();
    capture.await.unwrap();
    let result = match early {
        Ok(done) => done.unwrap(),
        Err(_) => forget.await.unwrap(),
    };
    assert_eq!(result, Ok(true));
    assert_eq!(
        registry.len(),
        0,
        "a forgotten login's stand-ins are still swapped"
    );
    assert_eq!(logins.kept(), []);
    assert_eq!(store.inner.ids(), Vec::<String>::new());
}
