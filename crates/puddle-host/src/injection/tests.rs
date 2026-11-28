// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use puddle_proxy::{InjectContext, InjectDecision, NoInjection, RequestView};
use puddle_secrets::{
    AccountName, Fetched, HostName, MemoryStore, Secret, SourceError, SourceSpec, StoredId,
};
use puddle_store::{
    Author, Clock, Coverage, CredentialBinding, EnvDraft, EnvName, EnvScope, IdentityDraft, Limits,
    ManualClock, Owner, SecretHost,
};
use puddle_types::{Host, PassThroughCode, ReasonCode};
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;
fn store() -> Arc<Store> {
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(1_000));
    owned(Arc::new(
        Store::open_in_memory(clock, Limits::default()).unwrap(),
    ))
}
/// `store` with the workspaces these tests use, each the owner's.
fn owned(store: Arc<Store>) -> Arc<Store> {
    for name in [
        "alpha", "beta", "free", "gone", "other", "running", "stopped", "stuck",
    ] {
        puddle_store::testing::own(&store, &workspace(name));
    }
    store
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
/// The handle of the workspace for its one test user, as the API makes it.
fn handle(ws: &WorkspaceId) -> puddle_services::owned::OwnedWorkspace {
    puddle_services::owned::OwnedWorkspace::for_test(puddle_store::UserId::OWNER, ws)
}
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
#[derive(Default)]
struct Events(Mutex<Vec<Event>>);
impl EventSink for Events {
    fn emit(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

mod ca_and_termination;
mod captured_logins;
mod environment_secrets;
mod git_injector;
mod learned_hosts;
mod sign_in_notices;
mod ssh_host_keys;
