// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use puddle_proxy::{InjectContext, InjectDecision, NoInjection, RequestView};
use puddle_secrets::{
    AccountName, Fetched, HostName, MemoryStore, Secret, SourceError, SourceSpec, StoredId,
};
use puddle_store::{
    Author, Clock, Coverage, CredentialBinding, EnvDraft, EnvName, EnvScope, IdentityDraft, Limits,
    ManualClock, Owner, SecretHost,
};
use puddle_types::Host;

use super::*;

fn store() -> Arc<Store> {
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(1_000));
    Arc::new(Store::open_in_memory(clock, Limits::default()).unwrap())
}

fn workspace(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

fn inputs(store: &Arc<Store>) -> InjectorInputs {
    inputs_with(store, Arc::new(Events::default()))
}

fn inputs_with(store: &Arc<Store>, events: Arc<Events>) -> InjectorInputs {
    InjectorInputs {
        store: Arc::clone(store),
        secrets: Arc::new(SecretCache::new(Sources::new(
            puddle_secrets::ToolPaths::resolve(),
            Arc::new(MemoryStore::new()),
        ))),
        events,
    }
}

/// What the user was told, in order.
fn told(events: &Events) -> Vec<Event> {
    events.0.lock().unwrap().clone()
}

fn injection(store: &Arc<Store>) -> (Injection, Arc<Terminations>) {
    let (injection, terminations, _) = injection_with_vault(store);
    (injection, terminations)
}

fn injection_with_vault(store: &Arc<Store>) -> (Injection, Arc<Terminations>, Arc<MemoryStore>) {
    let (injection, terminations, vault, _) = injection_with_events(store);
    (injection, terminations, vault)
}

fn injection_with_events(
    store: &Arc<Store>,
) -> (Injection, Arc<Terminations>, Arc<MemoryStore>, Arc<Events>) {
    let terminations = Arc::new(Terminations::new());
    let vault = Arc::new(MemoryStore::new());
    let events = Arc::new(Events::default());
    let injection = Injection::new(
        Arc::clone(&terminations),
        inputs_with(store, Arc::clone(&events)),
        None,
        vault.clone(),
    );
    (injection, terminations, vault, events)
}

const REAL: &str = "real-value-CANARY-4711";

/// A secret for `hosts` in the workspace's own scope, its value in `vault`.
fn add_secret(store: &Store, vault: &MemoryStore, ws: &WorkspaceId, var: &str, hosts: &[&str]) {
    let id = StoredId::new(format!("env-{var}")).unwrap();
    vault.set(&id, &Secret::new(REAL.to_owned())).unwrap();
    let hosts = hosts.iter().map(|h| SecretHost::new(h).unwrap()).collect();
    store
        .set_env(
            puddle_store::UserId::OWNER,
            &EnvScope::Workspace(ws.clone()),
            &EnvName::new(var).unwrap(),
            EnvDraft::secret(id, hosts).unwrap(),
        )
        .unwrap();
}

fn stand_ins(terminations: &Terminations, ws: &WorkspaceId) -> Vec<String> {
    terminations.termination(ws).unwrap().stand_ins().ids()
}

fn attach(store: &Store, ws: &WorkspaceId, label: &str, host: &str) {
    let host = HostName::new(host).unwrap();
    let credential = CredentialBinding::new(
        &host,
        SourceSpec::Gh {
            host: host.clone(),
            account: AccountName::new("me").unwrap(),
        },
        Coverage::new(BTreeSet::<Owner>::new(), true).unwrap(),
    )
    .unwrap();
    let identity = store
        .create_identity(
            puddle_store::UserId::OWNER,
            IdentityDraft {
                label: label.to_owned(),
                author: Author::new(label, &format!("{label}@example.org")).unwrap(),
                credentials: vec![credential],
            },
        )
        .unwrap();
    store.attach_identity(ws, identity.id, None).unwrap();
}

fn decrypts(terminations: &Terminations, ws: &WorkspaceId, host: &str) -> bool {
    terminations
        .termination(ws)
        .is_some_and(|t| t.set().contains(&Host::parse_normalised(host).unwrap()))
}

/// An injection that makes SSH host keys at each start.
fn injection_with_ssh(store: &Arc<Store>) -> (Injection, Arc<Terminations>) {
    let (injection, terminations) = injection(store);
    (injection.with_git_ssh(true), terminations)
}

fn served_keys(terminations: &Terminations, ws: &WorkspaceId) -> Vec<String> {
    let termination = terminations.termination(ws).unwrap();
    termination
        .git_ssh_keys()
        .map(|keys| {
            keys.keys()
                .iter()
                .map(|k| k.public_key().to_openssh().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_workspace_warns_about_a_foreign_credential_by_its_setting_read_each_time() {
    let store = store();
    let ws = workspace("alpha");
    let (injection, _) = injection(&store);
    // Without settings to read the warning is on.
    assert!(injection.foreign_warning(&ws).is_on());
    let repo = Arc::new(puddle_api::MemorySettings::default());
    let injection = injection.with_settings(repo.clone());
    let warning = injection.foreign_warning(&ws);
    assert!(warning.is_on());
    puddle_api::SettingsRepo::save_workspace(
        repo.as_ref(),
        &ws,
        serde_json::json!({"overrides": {"warn_foreign_credentials": false}}),
    )
    .unwrap();
    assert!(!warning.is_on(), "a change applies to the next request");
    assert!(injection.foreign_warning(&workspace("beta")).is_on());
}

#[tokio::test]
async fn git_over_ssh_off_makes_no_host_keys_but_still_names_the_ssh_hosts_so_they_are_refused() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    attach(&store, &ws, "ada", "git.corp.example");
    let began = injection.begin(&ws, false).await.unwrap();
    assert!(began.ssh.is_none());
    let termination = terminations.termination(&ws).unwrap();
    assert!(termination.git_ssh_keys().is_none());
    assert!(
        termination
            .git_ssh_hosts()
            .contains(&Host::parse_normalised("git.corp.example").unwrap())
    );
}

#[tokio::test]
async fn each_start_makes_new_host_keys_the_workspace_serves_and_the_guest_lists() {
    let store = store();
    let (injection, terminations) = injection_with_ssh(&store);
    let (a, b) = (workspace("alpha"), workspace("beta"));
    attach(&store, &a, "ada", "git.corp.example");
    let first = injection.begin(&a, false).await.unwrap();
    let other = injection.begin(&b, false).await.unwrap();

    // The keys served are the ones listed, under the built-in names and the credential's.
    let listing = first.ssh.unwrap();
    let served = served_keys(&terminations, &a);
    assert_eq!(served.len(), 3);
    let lines = listing.lines().unwrap();
    for key in &served {
        let half = key.split(' ').take(2).collect::<Vec<_>>().join(" ");
        for name in ["github.com", "ssh.dev.azure.com", "git.corp.example"] {
            assert!(lines.contains(&format!("{name} {half}")), "{name}");
            assert!(lines.contains(&format!("[{name}]:443 {half}")), "{name}");
        }
    }
    let ssh_hosts = terminations.termination(&a).unwrap();
    assert!(
        ssh_hosts
            .git_ssh_hosts()
            .contains(&Host::parse_normalised("git.corp.example").unwrap())
    );
    // Another workspace has other keys, and a restart of the first makes new ones.
    assert_ne!(served_keys(&terminations, &b), served);
    assert!(other.ssh.is_some());
    injection.end(&a);
    injection.begin(&a, false).await.unwrap();
    assert_ne!(served_keys(&terminations, &a), served);
}

#[tokio::test]
async fn a_change_to_the_identities_keeps_the_keys_and_changes_the_names() {
    let store = store();
    let (injection, terminations) = injection_with_ssh(&store);
    let ws = workspace("alpha");
    let began = injection.begin(&ws, false).await.unwrap();
    let before = served_keys(&terminations, &ws);
    assert!(
        !began
            .ssh
            .unwrap()
            .lines()
            .unwrap()
            .contains("git.corp.example")
    );
    attach(&store, &ws, "ada", "git.corp.example");
    injection.resync(&ws).unwrap();
    assert_eq!(served_keys(&terminations, &ws), before);
    assert!(
        terminations
            .termination(&ws)
            .unwrap()
            .git_ssh_hosts()
            .contains(&Host::parse_normalised("git.corp.example").unwrap())
    );
}

#[tokio::test]
async fn a_sandbox_gets_its_own_ca_and_decrypts_what_its_identities_name() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let (a, b) = (workspace("alpha"), workspace("beta"));
    attach(&store, &a, "ada", "github.com");

    let began_a = injection.begin(&a, false).await.unwrap();
    let began_b = injection.begin(&b, false).await.unwrap();
    assert_ne!(began_a.certificate, began_b.certificate);
    assert!(decrypts(&terminations, &a, "github.com"));
    assert!(!decrypts(&terminations, &a, "gitlab.com"));
    // beta has no identity: a CA (the guest trusts it from boot) and nothing decrypted.
    assert!(!decrypts(&terminations, &b, "github.com"));
    assert_eq!(injection.running_workspaces(), [a.clone(), b]);
    assert_eq!(began_a.git.identities.len(), 1);
}

#[tokio::test]
async fn refresh_applies_a_new_host_to_the_running_workspace_with_the_same_ca() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    injection.begin(&ws, false).await.unwrap();
    let ca_before = terminations
        .termination(&ws)
        .unwrap()
        .ca()
        .certificate()
        .clone();
    assert!(!decrypts(&terminations, &ws, "gitlab.com"));

    attach(&store, &ws, "ada", "gitlab.com");
    assert!(injection.resync(&ws).unwrap().is_some());
    assert!(decrypts(&terminations, &ws, "gitlab.com"));
    // The CA the guest was given at boot is the one that still signs.
    let ca_after = terminations
        .termination(&ws)
        .unwrap()
        .ca()
        .certificate()
        .clone();
    assert_eq!(ca_before, ca_after);
}

#[tokio::test]
async fn the_rules_on_method_and_path_follow_the_store_and_their_hosts_are_decrypted() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    let add = |workspace: Option<&WorkspaceId>, host: &str, path: &str| {
        store
            .add_request_rule(&puddle_store::NewRequestRule {
                workspace: workspace.cloned(),
                host: host.to_owned(),
                methods: puddle_types::Methods::parse(["DELETE"]).unwrap(),
                path: puddle_types::PathPattern::parse(path).unwrap(),
                created_by: puddle_store::Actor::Api,
            })
            .unwrap()
    };
    let delete_refused = |t: &Terminations, h: &str, path: &str| {
        let headers: Vec<String> = Vec::new();
        let view = RequestView::new("DELETE", path, &headers);
        t.termination(&ws)
            .unwrap()
            .request_rules()
            .refusing(&host(h), &view)
    };
    add(None, "global.example", "/a");
    add(Some(&workspace("beta")), "beta.example", "/a");
    // Too wide for the proxy's grammar: the row is left out, the others still apply.
    add(Some(&ws), "*.com", "/a");
    let own = add(Some(&ws), "own.example", "/b/**");
    injection.begin(&ws, false).await.unwrap();
    assert!(delete_refused(&terminations, "global.example", "/a").is_some());
    assert_eq!(
        delete_refused(&terminations, "own.example", "/b/c").map(|id| id.0),
        Some(own.id)
    );
    assert!(
        delete_refused(&terminations, "beta.example", "/a").is_none(),
        "another workspace's rule does not apply"
    );
    assert!(delete_refused(&terminations, "own.example", "/other").is_none());
    let decrypted = terminations.termination(&ws).unwrap();
    assert!(decrypted.set().contains(&host("own.example")));
    assert!(decrypted.set().contains(&host("global.example")));
    assert!(!decrypted.set().contains(&host("beta.example")));

    store
        .delete_request_rule(own.id, puddle_store::Actor::Api)
        .unwrap();
    add(Some(&ws), "new.example", "/n");
    injection.resync(&ws).unwrap();
    assert!(delete_refused(&terminations, "own.example", "/b/c").is_none());
    assert!(delete_refused(&terminations, "new.example", "/n").is_some());
}

#[tokio::test]
async fn a_workspace_on_the_curated_mode_decrypts_unlisted_hosts_and_passes_the_curated_ones() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::AllExceptCurated))
        .unwrap();
    injection.begin(&ws, false).await.unwrap();
    let interception = terminations
        .termination(&ws)
        .unwrap()
        .interception()
        .clone();
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    assert!(interception.is_decrypting_all());
    assert_eq!(interception.skipped(&host("unlisted.example")), None);
    for name in CURATED_PASS_THROUGH.iter().filter(|n| !n.starts_with("*.")) {
        assert_eq!(interception.skipped(&host(name)), Some("curated"), "{name}");
    }
}

#[tokio::test]
async fn a_stored_never_decrypt_name_that_does_not_parse_is_skipped_and_the_others_still_apply() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::All))
        .unwrap();
    // The API refuses such a name; a row written by another version or by hand must not
    // switch the workspace off.
    store
        .set_never_decrypt(&ws, &["not a host".to_owned(), "bank.example".to_owned()])
        .unwrap();
    injection.begin(&ws, false).await.unwrap();
    let interception = terminations
        .termination(&ws)
        .unwrap()
        .interception()
        .clone();
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    assert_eq!(
        interception.skipped(&host("bank.example")),
        Some("never_decrypt")
    );
    assert_eq!(interception.skipped(&host("other.example")), None);
}

#[tokio::test]
async fn the_switch_and_the_never_decrypt_names_apply_to_the_running_workspace_and_keep_what_was_learned()
 {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    let mode = |t: &Terminations| {
        let interception = t.termination(&ws).unwrap().interception().clone();
        (
            interception.is_decrypting_all(),
            interception.skipped(&host("bank.example")),
            interception.skipped(&host("global.example")),
        )
    };
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::All))
        .unwrap();
    store
        .set_never_decrypt(&ws, &["bank.example".to_owned()])
        .unwrap();
    store
        .set_global_never_decrypt(&["global.example".to_owned()])
        .unwrap();
    injection.begin(&ws, false).await.unwrap();
    assert_eq!(
        mode(&terminations),
        (true, Some("never_decrypt"), Some("never_decrypt")),
        "read at the start"
    );

    let learned = Arc::clone(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
    );
    learned.learn(&host("pinned.example"));
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::KnownOnly))
        .unwrap();
    assert!(injection.resync(&ws).unwrap().is_some());
    assert_eq!(mode(&terminations), (false, None, None), "switched off");
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::All))
        .unwrap();
    store.set_never_decrypt(&ws, &[]).unwrap();
    injection.resync(&ws).unwrap();
    assert_eq!(mode(&terminations), (true, None, Some("never_decrypt")));
    let after = terminations.termination(&ws).unwrap();
    assert!(
        after
            .interception()
            .learned()
            .contains(&host("pinned.example")),
        "a change to the settings does not forget what the tools refused"
    );
    assert!(Arc::ptr_eq(after.interception().learned(), &learned));
}

#[tokio::test]
async fn a_host_a_tool_refused_is_announced_with_its_count_and_listed_with_its_history() {
    use puddle_api::LearnedService;

    let store = store();
    let (injection, terminations, _, events) = injection_with_events(&store);
    let service = crate::learned::HostLearned::new(Arc::clone(&terminations), Arc::clone(&store));
    let ws = workspace("alpha");
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    assert_eq!(service.list(&ws).len(), 0, "nothing learned yet");
    assert!(!service.forget(&ws, "pinned.example"));
    assert_eq!(service.clear(&ws), 0);
    assert!(!service.discard(&ws, "pinned.example"));

    injection.begin(&ws, false).await.unwrap();
    let learned = Arc::clone(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
    );
    learned.learn(&host("pinned.example"));
    learned.learn(&host("pinned.example"));
    learned.learn(&host("other.example"));
    let learned_event = |h: &str, count: u32| Event::PassThroughLearned {
        workspace: ws.clone(),
        host: h.to_owned(),
        count,
    };
    assert_eq!(
        told(&events),
        [
            learned_event("pinned.example", 1),
            learned_event("other.example", 1)
        ],
        "each new host once, renewing says nothing"
    );

    let listed = service.list(&ws);
    assert_eq!(listed.len(), 2);
    assert!(
        listed.iter().all(|h| h.passed_through
            && h.count == 1
            && h.first_rejected_at == h.last_rejected_at
            && h.expires_in_secs
                .is_some_and(|s| (43_199..=43_200).contains(&s))),
        "{listed:?}: the time is the built-in twelve hours, rounded up"
    );
    assert!(!service.forget(&ws, "never.example"));
    assert!(!service.forget(&ws, "not a host"));
    assert!(service.forget(&ws, "pinned.example"));
    assert!(!learned.contains(&host("pinned.example")));
    assert!(
        !service.forget(&ws, "pinned.example"),
        "no longer passed through"
    );

    let after = service.list(&ws);
    assert_eq!(
        after
            .iter()
            .map(|h| (h.host.as_str(), h.passed_through))
            .collect::<Vec<_>>(),
        [("other.example", true), ("pinned.example", false)],
        "the host decrypted again stays listed with its history, after the passed through one"
    );

    // It refuses again: the count rises and the user is told again.
    learned.learn(&host("pinned.example"));
    assert_eq!(
        told(&events).last(),
        Some(&learned_event("pinned.example", 2)),
        "the count rises with each refusal after the host was decrypted again"
    );
    assert_eq!(
        service
            .list(&ws)
            .iter()
            .find(|h| h.host == "pinned.example")
            .map(|h| h.count),
        Some(2)
    );

    assert_eq!(service.clear(&ws), 2);
    assert_eq!(service.list(&ws).len(), 2, "clearing keeps the history");
    assert!(service.discard(&ws, "pinned.example"));
    assert!(!service.discard(&ws, "pinned.example"));
    assert_eq!(service.list(&ws).len(), 1);
    assert!(!learned.contains(&host("other.example")));
    assert!(service.discard(&ws, "other.example"));
    assert_eq!(service.list(&ws).len(), 0);
}

#[tokio::test]
async fn a_store_that_cannot_keep_the_history_still_passes_the_host_through_and_lists_it() {
    use puddle_api::LearnedService;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store =
        Arc::new(Store::open(&path, Arc::new(ManualClock::new(1_000)), Limits::default()).unwrap());
    let (injection, terminations, _, events) = injection_with_events(&store);
    let service = crate::learned::HostLearned::new(Arc::clone(&terminations), Arc::clone(&store));
    let ws = workspace("alpha");
    injection.begin(&ws, false).await.unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE learned_hosts")
        .unwrap();

    let learned = Arc::clone(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
    );
    learned.learn(&Host::parse_normalised("pinned.example").unwrap());
    assert!(
        learned.contains(&Host::parse_normalised("pinned.example").unwrap()),
        "the host is passed through whether or not its history was written"
    );
    assert_eq!(
        told(&events),
        [Event::PassThroughLearned {
            workspace: ws.clone(),
            host: "pinned.example".to_owned(),
            count: 1,
        }],
        "the user is told, with a count of one"
    );
    let listed = service.list(&ws);
    assert_eq!(
        listed
            .iter()
            .map(|h| (h.host.as_str(), h.passed_through, h.count))
            .collect::<Vec<_>>(),
        [("pinned.example", true, 1)],
        "a host passed through is listed even without its history"
    );
    assert!(
        !service.discard(&ws, "not a host"),
        "not a name: nothing to discard"
    );
    assert!(
        service.discard(&ws, "pinned.example"),
        "it was passed through, so it is discarded although its history cannot be dropped"
    );
    assert!(!learned.contains(&Host::parse_normalised("pinned.example").unwrap()));
}

#[tokio::test]
async fn the_history_of_a_refused_host_outlives_the_workspace_run_but_the_pass_through_does_not() {
    use puddle_api::LearnedService;

    let store = store();
    let (injection, terminations) = injection(&store);
    let service = crate::learned::HostLearned::new(Arc::clone(&terminations), Arc::clone(&store));
    let ws = workspace("alpha");
    injection.begin(&ws, false).await.unwrap();
    terminations
        .termination(&ws)
        .unwrap()
        .interception()
        .learned()
        .learn(&Host::parse_normalised("pinned.example").unwrap());
    assert!(service.list(&ws)[0].passed_through);
    injection.end(&ws);
    let kept = service.list(&ws);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (
            kept[0].host.as_str(),
            kept[0].passed_through,
            kept[0].expires_in_secs,
            kept[0].count
        ),
        ("pinned.example", false, None, 1),
        "a stopped workspace passes nothing through but remembers"
    );
    injection.begin(&ws, false).await.unwrap();
    assert!(
        !service.list(&ws)[0].passed_through,
        "a start begins with none passed through"
    );
}

#[tokio::test]
async fn the_learned_limits_reach_a_running_workspace_without_forgetting_what_it_learned() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    injection.begin(&ws, false).await.unwrap();
    let learned = Arc::clone(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
    );
    assert_eq!(
        (learned.ttl(), learned.cap()),
        (Duration::from_hours(12), 1024),
        "the built-in limits"
    );
    for n in 0..3 {
        learned.learn(&host(&format!("h{n}.example")));
    }
    store
        .set_learned_limits(puddle_store::LearnedLimits {
            ttl_secs: 600,
            cap: 2,
        })
        .unwrap();
    injection.resync(&ws).unwrap();
    assert_eq!(
        (learned.ttl(), learned.cap()),
        (Duration::from_secs(600), 2)
    );
    assert_eq!(learned.list().len(), 2, "the oldest made room");
    assert!(Arc::ptr_eq(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
        &learned
    ));
}

#[test]
fn the_stored_defaults_match_the_proxy_built_in_values() {
    let built_in = puddle_store::LearnedLimits::default();
    assert_eq!(
        Duration::from_secs(u64::from(built_in.ttl_secs)),
        puddle_proxy::LEARNED_TTL
    );
    assert_eq!(built_in.cap as usize, puddle_proxy::LEARNED_CAP);
}

#[tokio::test]
async fn a_stopped_sandbox_has_no_ca_and_decrypts_nothing() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    attach(&store, &ws, "ada", "github.com");
    injection.begin(&ws, false).await.unwrap();
    injection.end(&ws);
    assert!(terminations.termination(&ws).is_none());
    assert_eq!(injection.running_workspaces(), []);
    // A change to a workspace that is not running touches nothing.
    assert!(injection.resync(&ws).unwrap().is_none());
    assert!(terminations.termination(&ws).is_none());
    injection.end(&ws);
}

#[tokio::test]
async fn a_new_start_replaces_the_ca_of_the_earlier_one() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    let first = injection.begin(&ws, false).await.unwrap().certificate;
    let second = injection.begin(&ws, false).await.unwrap().certificate;
    assert_ne!(first, second);
    assert_eq!(
        terminations.termination(&ws).unwrap().ca().certificate(),
        &second
    );
}

#[tokio::test]
async fn the_longest_workspace_id_still_gives_a_valid_ca_name() {
    let store = store();
    let (injection, _) = injection(&store);
    let ws = workspace(&"a".repeat(WorkspaceId::MAX_LEN));
    assert!(injection.begin(&ws, false).await.is_ok());
}

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

/// A source that always needs the user to sign in.
struct NotSignedIn(Mutex<usize>);

impl Fetch for NotSignedIn {
    fn fetch(&self, _: &SourceSpec) -> impl Future<Output = Result<Fetched, SourceError>> + Send {
        *self.0.lock().unwrap() += 1;
        std::future::ready(Err(SourceError::NotSignedIn))
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<Event>>);

impl EventSink for Events {
    fn emit(&self, event: Event) {
        self.0.lock().unwrap().push(event);
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

#[tokio::test]
async fn without_a_factory_a_workspace_gets_puddles_git_injector_which_reads_the_store() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    let termination = terminations.termination(&ws).unwrap();
    assert!(format!("{termination:?}").contains("GitInjector"));
    // A push to a repository that is not on the workspace's list is refused, so the injector
    // read the workspace's settings from the store.
    let host = Host::parse_normalised("github.com").unwrap();
    let context = InjectContext {
        workspace: &ws,
        host: &host,
    };
    let lines = ["host: github.com".to_owned()];
    let view = RequestView::new(
        "GET",
        "/acme/web.git/info/refs?service=git-receive-pack",
        &lines,
    );
    let injector = Arc::clone(&injection.running().get(&ws).unwrap().injector);
    let decision = injector.decide(&context, &view).await;
    assert!(
        matches!(&decision, InjectDecision::Refuse(refusal) if refusal.code() == "push_denied")
    );
}

#[tokio::test]
async fn each_start_gets_an_injector_made_for_its_workspace_and_it_stays_across_changes() {
    let store = store();
    let made = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&made);
    let factory: InjectorFactory = Arc::new(move |_, workspace| {
        record.lock().unwrap().push(workspace.clone());
        Arc::new(NoInjection)
    });
    let terminations = Arc::new(Terminations::new());
    let injection = Injection::new(
        Arc::clone(&terminations),
        inputs(&store),
        Some(factory),
        Arc::new(MemoryStore::new()),
    );
    let (a, b) = (workspace("alpha"), workspace("beta"));
    injection.begin(&a, false).await.unwrap();
    injection.begin(&b, false).await.unwrap();
    assert_eq!(*made.lock().unwrap(), [a.clone(), b]);
    // A change to a running workspace keeps its injector (it may hold state of its own).
    let before = format!("{:?}", terminations.termination(&a).unwrap());
    attach(&store, &a, "ada", "github.com");
    assert!(injection.resync(&a).unwrap().is_some());
    assert_eq!(made.lock().unwrap().len(), 2);
    assert!(format!("{:?}", terminations.termination(&a).unwrap()).contains("NoInjection"));
    assert!(before.contains("NoInjection"));
    // A restart makes a new one.
    injection.begin(&a, false).await.unwrap();
    assert_eq!(made.lock().unwrap().len(), 3);
}

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
    assert!(answer.wants(&head));
    answer
        .rewrite(&head, body.as_bytes())
        .await
        .map(|bytes| bytes.to_vec())
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
