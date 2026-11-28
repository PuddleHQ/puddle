// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn a_start_registers_the_stand_ins_of_its_secrets_and_decrypts_their_hosts() {
    let store = store();
    let (injection, terminations, vault) = injection_with_vault(&store);
    let ws = workspace("alpha");
    add_secret(
        &store,
        &vault,
        &ws,
        "NPM_TOKEN",
        &["registry.npmjs.org", "*.example.com"],
    );
    store
        .set_env(
            puddle_store::UserId::OWNER,
            &EnvScope::Workspace(ws.clone()),
            &EnvName::new("EDITOR").unwrap(),
            EnvDraft::plain("vim").unwrap(),
        )
        .unwrap();

    let began = injection.begin(&ws, false).await.unwrap();
    assert_eq!(began.env.get("EDITOR"), Some("vim"));
    let stand_in = began.env.get("NPM_TOKEN").unwrap();
    assert!(stand_in.starts_with("puddle-secret-NPM_TOKEN-"));
    assert!(began.env.iter().all(|(_, v)| !v.contains("CANARY")));
    assert_eq!(stand_ins(&terminations, &ws), ["stand-in:secret:NPM_TOKEN"]);
    assert!(decrypts(&terminations, &ws, "registry.npmjs.org"));
    assert!(decrypts(&terminations, &ws, "api.example.com"));
    assert!(!decrypts(&terminations, &ws, "github.com"));
}
#[tokio::test]
async fn a_secret_added_to_a_running_workspace_is_decrypted_from_the_next_connection_with_no_restart()
 {
    let store = store();
    let (injection, terminations, vault) = injection_with_vault(&store);
    let ws = workspace("alpha");
    let first = injection.begin(&ws, false).await.unwrap();
    let before = terminations.termination(&ws).unwrap();
    assert_eq!(stand_ins(&terminations, &ws), Vec::<String>::new());
    assert!(!decrypts(&terminations, &ws, "api.example.org"));

    add_secret(&store, &vault, &ws, "API_KEY", &["api.example.org"]);
    injection.environment_changed(&ws).await;

    assert!(decrypts(&terminations, &ws, "api.example.org"));
    assert_eq!(stand_ins(&terminations, &ws), ["stand-in:secret:API_KEY"]);
    let after = terminations.termination(&ws).unwrap();
    // The same CA (the guest's trust is unchanged), the same registry (open connections see
    // the new stand-in too), a new set for the next connection.
    assert_eq!(before.ca().certificate(), after.ca().certificate());
    assert!(Arc::ptr_eq(before.stand_ins(), after.stand_ins()));
    assert_eq!(first.certificate, *after.ca().certificate());
    // Nothing was started again: the start made one CA.
    assert_eq!(injection.running_workspaces(), std::slice::from_ref(&ws));

    // A change of the hosts moves the decrypted host; removing the secret drops it.
    add_secret(&store, &vault, &ws, "API_KEY", &["other.example.org"]);
    injection.environment_changed(&ws).await;
    assert!(decrypts(&terminations, &ws, "other.example.org"));
    assert!(!decrypts(&terminations, &ws, "api.example.org"));
    store
        .delete_env(
            puddle_store::UserId::OWNER,
            &EnvScope::Workspace(ws.clone()),
            &EnvName::new("API_KEY").unwrap(),
        )
        .unwrap();
    injection.environment_changed(&ws).await;
    assert_eq!(stand_ins(&terminations, &ws), Vec::<String>::new());
    assert!(!decrypts(&terminations, &ws, "other.example.org"));
}
#[tokio::test]
async fn a_global_secret_changes_what_every_running_workspace_decrypts_and_a_git_change_keeps_it() {
    let store = store();
    let (injection, terminations, vault) = injection_with_vault(&store);
    let (a, b) = (workspace("alpha"), workspace("beta"));
    injection.begin(&a, false).await.unwrap();
    injection.begin(&b, false).await.unwrap();
    let id = StoredId::new("env-G").unwrap();
    vault.set(&id, &Secret::new(REAL.to_owned())).unwrap();
    store
        .set_env(
            puddle_store::UserId::OWNER,
            &EnvScope::Global,
            &EnvName::new("G").unwrap(),
            EnvDraft::secret(id, vec![SecretHost::new("g.example.org").unwrap()]).unwrap(),
        )
        .unwrap();
    for ws in [&a, &b] {
        injection.environment_changed(ws).await;
        assert!(decrypts(&terminations, ws, "g.example.org"), "{ws}");
    }
    // Another workspace's own stand-in differs: each holds its own.
    let held = |ws: &WorkspaceId| {
        store
            .env_for_start(ws, &mut |_| Err("already made".into()))
            .unwrap()
    };
    assert_ne!(held(&a), held(&b));

    // An identity attached later keeps the secret's host in the set.
    attach(&store, &a, "ada", "gitlab.com");
    injection.resync(&a).unwrap();
    assert!(decrypts(&terminations, &a, "gitlab.com"));
    assert!(decrypts(&terminations, &a, "g.example.org"));
    assert_eq!(stand_ins(&terminations, &a), ["stand-in:secret:G"]);
}
#[tokio::test]
async fn a_workspace_that_does_not_run_is_left_alone_and_a_failed_read_keeps_what_it_had() {
    let store = store();
    let (injection, terminations, vault, events) = injection_with_events(&store);
    let (stopped, running) = (workspace("stopped"), workspace("running"));
    add_secret(&store, &vault, &stopped, "T", &["x.example.org"]);
    vault.break_it();
    // Not running: nothing is read, nothing registered.
    injection.environment_changed(&stopped).await;
    assert!(terminations.termination(&stopped).is_none());

    vault.heal();
    injection.begin(&running, false).await.unwrap();
    add_secret(&store, &vault, &running, "T", &["x.example.org"]);
    injection.environment_changed(&running).await;
    assert!(decrypts(&terminations, &running, "x.example.org"));
    // The credential store fails while a second secret is added: the first stays as it was,
    // and the user is told which secret could not be read and why.
    assert_eq!(told(&events), []);
    add_secret(&store, &vault, &running, "U", &["y.example.org"]);
    vault.break_it();
    injection.environment_changed(&running).await;
    assert_eq!(stand_ins(&terminations, &running), ["stand-in:secret:T"]);
    assert!(!decrypts(&terminations, &running, "y.example.org"));
    let failures = told(&events);
    assert!(
        matches!(
            failures.as_slice(),
            [Event::WorkspaceEnvFailed { workspace, reason }]
                if workspace == &running && reason.contains("credential store") && !reason.contains(REAL)
        ),
        "{failures:?}"
    );
    vault.heal();
    injection.environment_changed(&running).await;
    assert_eq!(
        stand_ins(&terminations, &running),
        ["stand-in:secret:T", "stand-in:secret:U"]
    );
}
#[tokio::test]
async fn a_start_whose_secret_cannot_be_read_fails_with_the_reason_and_registers_nothing() {
    let store = store();
    let (injection, terminations, vault) = injection_with_vault(&store);
    let ws = workspace("alpha");
    add_secret(&store, &vault, &ws, "T", &["x.example.org"]);
    vault.break_it();
    let err = injection.begin(&ws, false).await.err().unwrap().to_string();
    assert!(
        err.contains('T') && err.contains("credential store"),
        "{err}"
    );
    assert!(terminations.termination(&ws).is_none());
    assert_eq!(injection.running_workspaces(), []);
}
#[tokio::test]
async fn a_deleted_workspace_loses_its_variables_and_the_values_of_its_own_secrets() {
    let store = store();
    let (injection, _, vault, events) = injection_with_events(&store);
    let (gone, other) = (workspace("gone"), workspace("other"));
    add_secret(&store, &vault, &gone, "MINE", &["x.example.org"]);
    add_secret(&store, &vault, &other, "THEIRS", &["x.example.org"]);
    let id = StoredId::new("env-GLOBAL").unwrap();
    vault.set(&id, &Secret::new(REAL.to_owned())).unwrap();
    store
        .set_env(
            puddle_store::UserId::OWNER,
            &EnvScope::Global,
            &EnvName::new("GLOBAL").unwrap(),
            EnvDraft::secret(id, vec![SecretHost::new("g.example.org").unwrap()]).unwrap(),
        )
        .unwrap();

    injection.forget(&gone).await;
    assert_eq!(vault.ids(), ["env-GLOBAL", "env-THEIRS"]);
    assert_eq!(
        store
            .env_entries(puddle_store::UserId::OWNER, &EnvScope::Workspace(gone))
            .unwrap(),
        []
    );
    assert_eq!(
        store
            .env_entries(puddle_store::UserId::OWNER, &EnvScope::Workspace(other))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .env_entries(puddle_store::UserId::OWNER, &EnvScope::Global)
            .unwrap()
            .len(),
        1
    );

    // A credential store that fails leaves the value but never stops the deletion, and the
    // user is told which secret's value stayed behind.
    assert_eq!(told(&events), []);
    vault.break_it();
    injection.forget(&workspace("other")).await;
    let failures = told(&events);
    assert!(
        matches!(
            failures.as_slice(),
            [Event::WorkspaceEnvFailed { workspace, reason }]
                if workspace == &self::workspace("other") && reason.contains("THEIRS")
        ),
        "{failures:?}"
    );
    assert_eq!(
        store
            .env_entries(
                puddle_store::UserId::OWNER,
                &EnvScope::Workspace(workspace("other"))
            )
            .unwrap(),
        []
    );
}
/// A credential store that does not answer for a while (a keyring waiting for the user to
/// unlock it) when asked for a secret, and says when it was asked. Saved logins are read
/// without delay: these tests are about secrets.
struct SlowStore {
    inner: MemoryStore,
    delay: Duration,
    asked: Mutex<Vec<String>>,
}
impl SecretStore for SlowStore {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, puddle_secrets::StoreError> {
        if !id.to_string().starts_with("login-") {
            self.asked.lock().unwrap().push(id.to_string());
            // Simulated slow store: every lookup takes `delay`.
            std::thread::sleep(self.delay);
        }
        self.inner.get(id)
    }

    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), puddle_secrets::StoreError> {
        self.inner.set(id, secret)
    }

    fn delete(&self, id: &StoredId) -> Result<(), puddle_secrets::StoreError> {
        self.inner.delete(id)
    }
}
#[tokio::test]
async fn a_credential_store_that_does_not_answer_fails_that_workspace_in_time_and_holds_up_no_other()
 {
    let store = store();
    let vault = Arc::new(SlowStore {
        inner: MemoryStore::new(),
        delay: Duration::from_millis(1500),
        asked: Mutex::new(Vec::new()),
    });
    let injection = Injection::new(
        Arc::new(Terminations::new()),
        inputs(&store),
        None,
        vault.clone(),
    )
    .with_vault_timeout(Duration::from_millis(200));
    let (stuck, free) = (workspace("stuck"), workspace("free"));
    add_secret(&store, &vault.inner, &stuck, "T", &["x.example.org"]);

    let waiting = injection.begin(&stuck, false);
    let others = async {
        // A workspace without secrets never asks the store for one, so it is not held up.
        let began = injection.begin(&free, false).await;
        assert!(began.is_ok());
    };
    let (stuck_result, ()) = tokio::join!(waiting, others);
    let err = stuck_result.err().unwrap().to_string();
    assert!(err.contains("did not answer within"), "{err}");
    assert!(err.contains("stuck") && err.contains("unlock"), "{err}");
    assert_eq!(vault.asked.lock().unwrap().len(), 1);
    assert_eq!(injection.running_workspaces(), [free]);
    // The store is a plain one otherwise.
    let id = StoredId::new("env-x").unwrap();
    vault.set(&id, &Secret::new("v".to_owned())).unwrap();
    vault.delete(&id).unwrap();
}
/// Two stand-ins of which one contains the other: the proxy cannot tell which to swap, so it
/// refuses to hold both. Puddle makes none like that; a hand-edited database can.
fn overlapping_stand_ins(store: &Store, ws: &WorkspaceId) {
    store
        .env_for_start(ws, &mut |name| {
            let first = "puddle-secret-A-0123456789abcdef0123456789abcdef";
            Ok(if name.as_str() == "A" {
                first.to_owned()
            } else {
                format!("{first}-{name}")
            })
        })
        .unwrap();
}
#[tokio::test]
async fn stand_ins_that_overlap_stop_a_start_and_leave_a_running_workspace_as_it_was() {
    let store = store();
    let (injection, terminations, vault) = injection_with_vault(&store);
    let ws = workspace("alpha");
    add_secret(&store, &vault, &ws, "A", &["a.example.org"]);
    add_secret(&store, &vault, &ws, "B", &["b.example.org"]);
    overlapping_stand_ins(&store, &ws);
    let err = injection.begin(&ws, false).await.err().unwrap().to_string();
    assert!(err.contains("cannot be registered"), "{err}");
    assert!(terminations.termination(&ws).is_none());

    // The same while it runs: it keeps the secret it had.
    let other = workspace("beta");
    add_secret(&store, &vault, &other, "A", &["a.example.org"]);
    overlapping_stand_ins(&store, &other);
    injection.begin(&other, false).await.unwrap();
    add_secret(&store, &vault, &other, "B", &["b.example.org"]);
    overlapping_stand_ins(&store, &other);
    injection.environment_changed(&other).await;
    assert_eq!(stand_ins(&terminations, &other), ["stand-in:secret:A"]);
    assert!(!decrypts(&terminations, &other, "b.example.org"));
}
