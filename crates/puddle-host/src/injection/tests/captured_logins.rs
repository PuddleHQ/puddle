// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

// ---------------------------------------------------------------------------------------
// Captured logins
/// What the proxy does with a Claude Code token answer, through the termination the host made.
async fn claude_answer(terminations: &Terminations, ws: &WorkspaceId) -> Option<Vec<u8>> {
    let termination = terminations.termination(ws).unwrap();
    let mut exchange = termination
        .exchanges()
        .unwrap()
        .begin(
            &Host::parse_normalised("platform.claude.com").unwrap(),
            "POST",
            "/v1/oauth/token",
        )
        .unwrap();
    let answer = exchange.answer_mut().unwrap();
    let body = serde_json::json!({
        "access_token": format!("sk-ant-oat01-CANARY-access-{}", "a".repeat(60)),
        "refresh_token": format!("sk-ant-ort01-CANARY-refresh-{}", "b".repeat(60)),
        "expires_in": 28800,
    })
    .to_string();
    let head = puddle_proxy::AnswerHead {
        status: 200,
        content_type: Some("application/json"),
        content_encoding: None,
        content_length: Some(body.len() as u64),
    };
    assert_eq!(answer.wants(&head), puddle_proxy::Want::Read);
    match answer.rewrite(&head, body.as_bytes()).await {
        puddle_proxy::Rewrite::Replace(bytes) => Some(bytes.to_vec()),
        _ => None,
    }
}
fn login_ids(terminations: &Terminations, ws: &WorkspaceId) -> Vec<String> {
    stand_ins(terminations, ws)
        .into_iter()
        .filter(|id| id.starts_with("stand-in:login:"))
        .collect()
}
#[tokio::test]
async fn with_capture_on_a_workspace_decrypts_every_login_host_and_reads_its_token_exchanges() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let (on, off) = (workspace("alpha"), workspace("beta"));
    injection.begin(&on, true).await.unwrap();
    injection.begin(&off, false).await.unwrap();
    for host in [
        "platform.claude.com",
        "api.anthropic.com",
        "github.com",
        "api.github.com",
        "api.individual.githubcopilot.com",
    ] {
        assert!(decrypts(&terminations, &on, host), "{host}");
        assert!(!decrypts(&terminations, &off, host), "{host}");
    }
    assert!(!decrypts(&terminations, &on, "example.com"));
    // The login hosts stay decrypted when the workspace's identities change.
    attach(&store, &on, "ada", "gitlab.com");
    assert!(injection.resync(&on).unwrap().is_some());
    assert!(decrypts(&terminations, &on, "platform.claude.com"));
    assert!(decrypts(&terminations, &on, "gitlab.com"));
    // Capture off reads nothing: the token endpoint is an ordinary request.
    let exchanges = |ws: &WorkspaceId| {
        terminations
            .termination(ws)
            .unwrap()
            .exchanges()
            .unwrap()
            .begin(
                &Host::parse_normalised("platform.claude.com").unwrap(),
                "POST",
                "/v1/oauth/token",
            )
            .is_some()
    };
    assert!(exchanges(&on));
    assert!(!exchanges(&off));
}
#[tokio::test]
async fn a_captured_login_shares_the_registry_with_secrets_and_outlives_a_restart() {
    let store = store();
    let (injection, terminations, vault) = injection_with_vault(&store);
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    let given = claude_answer(&terminations, &ws).await.unwrap();
    let given: serde_json::Value = serde_json::from_slice(&given).unwrap();
    assert!(
        given["access_token"]
            .as_str()
            .unwrap()
            .starts_with("sk-ant-oat01-")
    );
    assert!(!given.to_string().contains("CANARY"));
    assert_eq!(
        login_ids(&terminations, &ws),
        [
            "stand-in:login:claude-access",
            "stand-in:login:claude-refresh"
        ]
    );

    // A secret added to the running workspace replaces only the secrets' entries.
    add_secret(&store, &vault, &ws, "API_KEY", &["api.example.org"]);
    injection.environment_changed(&ws).await;
    assert_eq!(stand_ins(&terminations, &ws).len(), 3);
    assert_eq!(login_ids(&terminations, &ws).len(), 2);

    // The next start finds the login again, before the guest could use it.
    injection.end(&ws);
    assert!(terminations.termination(&ws).is_none());
    injection.begin(&ws, true).await.unwrap();
    assert_eq!(login_ids(&terminations, &ws).len(), 2);
    let again = claude_answer(&terminations, &ws).await.unwrap();
    let again: serde_json::Value = serde_json::from_slice(&again).unwrap();
    assert_eq!(
        again["access_token"], given["access_token"],
        "the slot keeps its stand-in"
    );

    // Another workspace has none of it.
    let other = workspace("beta");
    injection.begin(&other, true).await.unwrap();
    assert_eq!(login_ids(&terminations, &other).len(), 0);
}
#[tokio::test]
async fn with_capture_off_the_logins_kept_before_are_not_used_and_the_user_is_told_why() {
    let store = store();
    let (injection, terminations, vault, events) = injection_with_events(&store);
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    claude_answer(&terminations, &ws).await.unwrap();
    injection.end(&ws);
    assert!(events.0.lock().unwrap().is_empty());

    injection.begin(&ws, false).await.unwrap();
    assert_eq!(login_ids(&terminations, &ws).len(), 0);
    assert_eq!(
        *events.0.lock().unwrap(),
        [Event::LoginProblem {
            workspace: ws.clone(),
            service: "Claude Code".into(),
            kind: puddle_types::LoginProblemKind::CaptureOff,
        }]
    );

    // A store that does not answer is not a reason to say anything with capture off.
    injection.end(&ws);
    vault.break_it();
    injection.begin(&ws, false).await.unwrap();
    assert_eq!(events.0.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn with_capture_off_the_marker_answers_and_the_credential_store_is_not_asked() {
    let store = store();
    let dir = tempfile::tempdir().unwrap();
    let (injection, terminations, vault, events) = injection_with_events(&store);
    let injection = injection.with_login_markers(dir.path().to_owned());
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    claude_answer(&terminations, &ws).await.unwrap();
    injection.end(&ws);

    // A store that hangs or refuses would make a start without the marker say nothing, or wait.
    vault.break_it();
    injection.begin(&ws, false).await.unwrap();
    assert_eq!(events.0.lock().unwrap().len(), 1);
    // Another workspace has nothing kept, and the marker says so after its first start.
    let other = workspace("beta");
    injection.begin(&other, false).await.unwrap();
    assert_eq!(events.0.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn forgetting_a_login_ends_its_notice_and_in_a_running_workspace_its_stand_ins() {
    let store = store();
    let dir = tempfile::tempdir().unwrap();
    let (injection, terminations, vault, events) = injection_with_events(&store);
    let injection = injection.with_login_markers(dir.path().to_owned());
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    claude_answer(&terminations, &ws).await.unwrap();
    assert_eq!(login_ids(&terminations, &ws).len(), 2);
    assert_eq!(injection.forget_login(&ws, "azure").await, Ok(false));
    assert_eq!(injection.forget_login(&ws, "Claude Code").await, Ok(true));
    assert_eq!(login_ids(&terminations, &ws).len(), 0);
    injection.end(&ws);

    // Nothing is kept now, so capture off has nothing to say, and asks nobody.
    vault.break_it();
    injection.begin(&ws, false).await.unwrap();
    assert!(events.0.lock().unwrap().is_empty());
}
/// A store whose reads, once `hold` is called, return their value only after `release`.
struct HeldReads {
    inner: MemoryStore,
    state: Arc<(Mutex<(bool, usize)>, std::sync::Condvar)>,
}
impl HeldReads {
    fn new() -> Self {
        Self {
            inner: MemoryStore::new(),
            state: Arc::default(),
        }
    }

    fn hold(&self) {
        self.state.0.lock().unwrap().0 = true;
    }

    fn release(&self) {
        self.state.0.lock().unwrap().0 = false;
        self.state.1.notify_all();
    }

    /// How many reads have started while held.
    fn held_reads(&self) -> usize {
        self.state.0.lock().unwrap().1
    }
}
impl SecretStore for HeldReads {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, puddle_secrets::StoreError> {
        let read = self.inner.get(id);
        let mut guard = self.state.0.lock().unwrap();
        if guard.0 {
            guard.1 += 1;
            while guard.0 {
                guard = self.state.1.wait(guard).unwrap();
            }
        }
        read
    }

    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), puddle_secrets::StoreError> {
        self.inner.set(id, secret)
    }

    fn delete(&self, id: &StoredId) -> Result<(), puddle_secrets::StoreError> {
        self.inner.delete(id)
    }
}
#[tokio::test]
async fn a_forget_that_arrives_while_a_start_reads_the_kept_logins_ends_with_the_login_gone() {
    let store = store();
    let dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(HeldReads::new());
    let terminations = Arc::new(Terminations::new());
    let injection = Arc::new(
        Injection::new(
            Arc::clone(&terminations),
            inputs(&store),
            None,
            vault.clone(),
        )
        .with_login_markers(dir.path().to_owned()),
    );
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    claude_answer(&terminations, &ws).await.unwrap();
    injection.end(&ws);

    // The next start has read a kept token and waits on the store.
    vault.hold();
    let start = tokio::spawn({
        let (injection, ws) = (Arc::clone(&injection), ws.clone());
        async move { injection.begin(&ws, true).await.map(|_| ()) }
    });
    puddle_test_kit::eventually(
        "the start's read of a kept token",
        Duration::from_secs(5),
        || vault.held_reads() > 0,
    )
    .await;
    // The workspace is not running yet, so this forget has no registry to remove from.
    let forget = tokio::spawn({
        let (injection, ws) = (Arc::clone(&injection), ws.clone());
        async move { injection.forget_login(&ws, "claude").await }
    });
    // An absence window: a forget that does not wait for the start would finish in this time.
    tokio::time::sleep(Duration::from_millis(100)).await;
    vault.release();
    start.await.unwrap().unwrap();
    assert_eq!(forget.await.unwrap(), Ok(true));

    assert_eq!(login_ids(&terminations, &ws).len(), 0, "still swapped");
    assert_eq!(injection.kept_logins(&ws).await.logins.len(), 0);
}
#[tokio::test]
async fn the_list_of_kept_logins_follows_a_capture_and_a_forget_in_a_running_or_stopped_workspace()
{
    let store = store();
    let dir = tempfile::tempdir().unwrap();
    let (injection, terminations, vault, _events) = injection_with_events(&store);
    let injection = injection.with_login_markers(dir.path().to_owned());
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    assert_eq!(injection.kept_logins(&ws).await.logins.len(), 0);
    claude_answer(&terminations, &ws).await.unwrap();
    let running = injection.kept_logins(&ws).await;
    assert_eq!(running.logins.len(), 1);
    assert_eq!(running.logins[0].profile, "claude");
    assert!(running.logins[0].since_ms.is_some());
    injection.end(&ws);

    // Stopped, with a store that does not answer: the marker still says what is kept.
    vault.break_it();
    assert_eq!(injection.kept_logins(&ws).await, running);
    vault.heal();
    assert_eq!(injection.forget_login(&ws, "claude").await, Ok(true));
    assert_eq!(injection.kept_logins(&ws).await.logins.len(), 0);
}
#[tokio::test]
async fn a_stopped_workspace_forgets_a_login_kept_for_it_and_a_refusing_store_is_an_error() {
    let store = store();
    let dir = tempfile::tempdir().unwrap();
    let (injection, terminations, vault, events) = injection_with_events(&store);
    let injection = injection.with_login_markers(dir.path().to_owned());
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    claude_answer(&terminations, &ws).await.unwrap();
    injection.end(&ws);
    vault.break_it();
    assert!(injection.forget_login(&ws, "claude").await.is_err());
    // The notice stays: the login is still kept.
    injection.begin(&ws, false).await.unwrap();
    assert_eq!(events.0.lock().unwrap().len(), 1);
    injection.end(&ws);
    vault.heal();
    assert_eq!(injection.forget_login(&ws, "claude").await, Ok(true));
    injection.begin(&ws, false).await.unwrap();
    assert_eq!(events.0.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn deleting_a_workspace_deletes_its_marker_too() {
    let store = store();
    let dir = tempfile::tempdir().unwrap();
    let (injection, terminations, _vault, _events) = injection_with_events(&store);
    let injection = injection.with_login_markers(dir.path().to_owned());
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    claude_answer(&terminations, &ws).await.unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    injection.end(&ws);
    injection.forget(&ws).await;
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
#[tokio::test]
async fn a_credential_store_that_is_not_there_does_not_stop_a_start_and_says_so() {
    let store = store();
    let events = Arc::new(Events::default());
    let vault = Arc::new(MemoryStore::new());
    vault.break_it();
    let terminations = Arc::new(Terminations::new());
    let injection = Injection::new(
        Arc::clone(&terminations),
        InjectorInputs {
            events: events.clone(),
            ..inputs(&store)
        },
        None,
        vault,
    );
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    let sent = events.0.lock().unwrap().clone();
    let expected: Vec<Event> = puddle_logins::builtin()
        .iter()
        .map(|profile| Event::LoginProblem {
            workspace: ws.clone(),
            service: profile.name.to_owned(),
            kind: puddle_types::LoginProblemKind::StoreUnavailable,
        })
        .collect();
    assert_eq!(sent.len(), expected.len(), "{sent:?}");
    for event in &expected {
        assert!(sent.contains(event), "no notice {event:?} in {sent:?}");
    }
    // The login still works, as it would without Puddle: nothing was captured.
    assert!(claude_answer(&terminations, &ws).await.is_none());
}
#[tokio::test]
async fn deleting_a_workspace_deletes_the_logins_kept_for_it() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    claude_answer(&terminations, &ws).await.unwrap();
    injection.end(&ws);
    injection.forget(&ws).await;
    injection.begin(&ws, true).await.unwrap();
    assert_eq!(login_ids(&terminations, &ws).len(), 0);
}
#[tokio::test]
async fn each_failure_to_start_or_remove_names_its_workspace_and_what_broke() {
    let ws = workspace("a");
    let aborted = {
        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        task.await.unwrap_err()
    };
    let err = InjectionError::environment_task(&ws)(aborted);
    assert!(
        err.to_string().starts_with("cannot read a's environment: "),
        "{err}"
    );

    let aborted = {
        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        task.await.unwrap_err()
    };
    let err = InjectionError::host_keys_task(&ws)(aborted);
    assert!(
        err.to_string()
            .starts_with("cannot make a's SSH host keys: "),
        "{err}"
    );

    let err = InjectionError::authority(&ws)(puddle_ca::CaError::Expired);
    assert_eq!(
        err.to_string(),
        "cannot make a's certificate authority: the CA certificate has expired"
    );

    let broke = || puddle_store::StoreError::Backup("disk full".to_owned());
    let err = InjectionError::settings("the interception setting")(broke());
    assert_eq!(
        err.to_string(),
        "cannot read the interception setting: could not back up the database before migrating: disk full"
    );

    let err = InjectionError::removal(&ws)(broke());
    assert_eq!(
        err.to_string(),
        "cannot remove a's rules and environment: could not back up the database before migrating: disk full"
    );
}
