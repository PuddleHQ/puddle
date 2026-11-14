// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_secrets::{MemoryStore, Secret, StoredId};
use puddle_store::{EnvDraft, EnvName, EnvScope, Limits, ManualClock, SecretHost};

use super::*;

fn id(text: &str) -> StoredId {
    StoredId::new(text).unwrap()
}

fn put(vault: &dyn SecretStore, name: &str) {
    vault.set(&id(name), &Secret::new("value".into())).unwrap();
}

/// A shared memory store: one handle for the run that crashed, one for the run after it.
struct Shared(Arc<MemoryStore>);

impl SecretStore for Shared {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, puddle_secrets::StoreError> {
        self.0.get(id)
    }
    fn set(&self, id: &StoredId, s: &Secret) -> Result<(), puddle_secrets::StoreError> {
        self.0.set(id, s)
    }
    fn delete(&self, id: &StoredId) -> Result<(), puddle_secrets::StoreError> {
        self.0.delete(id)
    }
    fn keep_local(&self, id: &StoredId) -> Result<(), puddle_secrets::StoreError> {
        self.0.keep_local(id)
    }
}

#[test]
fn a_sweep_removes_what_no_identity_or_variable_names_and_keeps_the_rest() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    store
        .set_env(
            puddle_store::UserId::OWNER,
            &EnvScope::Global,
            &EnvName::new("TOKEN").unwrap(),
            EnvDraft::secret(
                id("env-named"),
                vec![SecretHost::new("a.example.com").unwrap()],
            )
            .unwrap(),
        )
        .unwrap();
    let disk = Arc::new(MemoryStore::new());
    let earlier = IndexedStore::new(Shared(disk.clone()));
    for name in ["env-named", "env-orphan", "tok-orphan"] {
        put(&earlier, name);
    }
    let later = IndexedStore::new(Shared(disk.clone()));
    assert_eq!(
        sweep(&later, &store),
        Ok(Sweep {
            removed: 2,
            stuck: 0
        })
    );
    assert!(later.get(&id("env-named")).unwrap().is_some());
    assert!(later.get(&id("env-orphan")).unwrap().is_none());
    assert!(later.get(&id("tok-orphan")).unwrap().is_none());
}

#[test]
fn a_sweep_keeps_the_proxy_sign_ins_which_no_row_names() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    let disk = Arc::new(MemoryStore::new());
    // An earlier run wrote a sign-in and an orphan; this run sweeps.
    let earlier = Arc::new(IndexedStore::new(Shared(disk.clone())));
    let passwords = crate::ProxyPasswords::new(earlier.clone());
    let proxy = puddle_upstream::ProxyAddr::new("proxy.corp", 8080);
    passwords
        .set(&proxy, "alice", &puddle_types::fixture::password("vault"))
        .unwrap();
    put(earlier.as_ref(), "tok-orphan");
    let later = Arc::new(IndexedStore::new(Shared(disk)));
    assert_eq!(
        sweep(later.as_ref(), &store),
        Ok(Sweep {
            removed: 1,
            stuck: 0
        })
    );
    let again = crate::ProxyPasswords::new(later);
    assert_eq!(again.load().unwrap(), 1);
}

#[test]
fn a_store_that_does_not_answer_is_left_alone_and_the_sweep_says_so() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    let disk = Arc::new(MemoryStore::new());
    put(&IndexedStore::new(Shared(disk.clone())), "tok-a");
    disk.break_it();
    assert_eq!(
        sweep(&IndexedStore::new(Shared(disk.clone())), &store),
        Err("the credential store did not answer")
    );
    disk.heal();
    assert!(disk.get(&id("tok-a")).unwrap().is_some());
}

#[test]
fn a_sweep_also_keeps_listed_and_named_values_on_this_computer() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    store
        .set_env(
            puddle_store::UserId::OWNER,
            &EnvScope::Global,
            &EnvName::new("TOKEN").unwrap(),
            EnvDraft::secret(
                id("env-named"),
                vec![SecretHost::new("a.example.com").unwrap()],
            )
            .unwrap(),
        )
        .unwrap();
    let disk = Arc::new(MemoryStore::new());
    put(&IndexedStore::new(Shared(disk.clone())), "env-listed");
    let later = IndexedStore::new(Shared(disk.clone()));
    sweep(&later, &store).unwrap();
    let kept = disk.kept_local();
    assert!(kept.contains(&"env-listed".to_owned()));
    assert!(kept.contains(&"env-named".to_owned()));
    assert!(kept.contains(&"puddle-index".to_owned()));
}

#[test]
fn a_store_that_cannot_be_rewritten_does_not_stop_the_sweep() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    let broken = MemoryStore::new();
    broken.break_it();
    keep_local(&broken, [id("a")].iter());
    assert_eq!(sweep(&broken, &store), Ok(Sweep::default()));
}

#[tokio::test]
async fn the_background_sweep_runs_off_the_async_threads() {
    let store =
        Arc::new(Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap());
    let disk = Arc::new(MemoryStore::new());
    put(&IndexedStore::new(Shared(disk.clone())), "tok-a");
    let sweep = sweep_in_background(
        Arc::new(IndexedStore::new(Shared(disk.clone()))),
        store,
        Arc::new(Problems::new(Arc::new(puddle_types::NullSink))),
    );
    sweep.await.unwrap();
    puddle_test_kit::eventually("the sweep ran", std::time::Duration::from_secs(5), || {
        disk.get(&id("tok-a")).unwrap().is_none()
    })
    .await;
}

/// A store that will not delete one id.
struct Stuck(Arc<MemoryStore>);

impl SecretStore for Stuck {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, puddle_secrets::StoreError> {
        self.0.get(id)
    }
    fn set(&self, id: &StoredId, s: &Secret) -> Result<(), puddle_secrets::StoreError> {
        self.0.set(id, s)
    }
    fn delete(&self, id: &StoredId) -> Result<(), puddle_secrets::StoreError> {
        if id.as_str() == "tok-stuck" {
            return Err(puddle_secrets::StoreError);
        }
        self.0.delete(id)
    }
}

fn problems() -> Problems {
    Problems::new(Arc::new(puddle_types::NullSink))
}

#[test]
fn a_sweep_that_leaves_a_value_behind_raises_a_problem_until_a_later_sweep_removes_it() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    let disk = Arc::new(MemoryStore::new());
    let earlier = IndexedStore::new(Shared(disk.clone()));
    put(&earlier, "tok-stuck");
    put(&earlier, "tok-gone");
    let list = problems();

    report(
        sweep(&IndexedStore::new(Stuck(disk.clone())), &store),
        &list,
    );
    let raised = list.list();
    assert_eq!(raised.len(), 1, "{raised:?}");
    assert_eq!(raised[0].key, SWEEP_PROBLEM);
    assert_eq!(
        raised[0].title,
        "Puddle could not remove 1 leftover value from the credential store"
    );
    assert!(disk.get(&id("tok-gone")).unwrap().is_none());

    // The store lets it go on the next start: the problem ends by itself.
    report(
        sweep(&IndexedStore::new(Shared(disk.clone())), &store),
        &list,
    );
    assert_eq!(list.list(), Vec::new());
    assert!(disk.get(&id("tok-stuck")).unwrap().is_none());
}

#[test]
fn a_sweep_that_cannot_run_raises_a_problem_naming_why_and_a_clean_one_raises_none() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    let disk = Arc::new(MemoryStore::new());
    put(&IndexedStore::new(Shared(disk.clone())), "tok-a");
    let list = problems();

    disk.break_it();
    report(
        sweep(&IndexedStore::new(Shared(disk.clone())), &store),
        &list,
    );
    let raised = list.list();
    assert_eq!(raised.len(), 1);
    assert!(raised[0].detail.contains("credential store did not answer"));
    disk.heal();

    report(
        sweep(&IndexedStore::new(Shared(disk.clone())), &store),
        &list,
    );
    assert_eq!(list.list(), Vec::new());
    report(sweep(&IndexedStore::new(Shared(disk)), &store), &list);
    assert_eq!(list.list(), Vec::new());
}

#[test]
fn a_store_that_refuses_a_write_raises_one_problem_naming_the_cause_and_the_restart() {
    let list = problems();
    for (health, title) in [
        (StoreHealth::NoProvider, "no credential store is running"),
        (StoreHealth::Locked, "the credential store is locked"),
    ] {
        report_health(health, true, &list);
        let raised = list.list();
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert_eq!(raised[0].key, STORE_PROBLEM);
        assert!(raised[0].title.contains(title), "{}", raised[0].title);
        assert!(raised[0].detail.contains("restart Puddle"));
    }
    assert!(list.list()[0].detail.contains("gnome-keyring-daemon"));
}

#[test]
fn a_working_store_raises_no_problem_and_clears_a_standing_one() {
    let list = problems();
    report_health(StoreHealth::Locked, false, &list);
    assert_eq!(list.list().len(), 1);
    report_health(StoreHealth::Working, false, &list);
    assert_eq!(list.list(), Vec::new());
}

#[tokio::test]
async fn the_startup_check_raises_for_a_locked_store_and_stays_quiet_for_a_healthy_one() {
    let locked = Arc::new(MemoryStore::new());
    locked.break_it();
    let list = Arc::new(problems());
    run_check(Probe::Store(locked), None, None, list.clone())
        .await
        .unwrap();
    let raised = list.list();
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].key, STORE_PROBLEM);

    let healthy = Arc::new(MemoryStore::new());
    let quiet = Arc::new(problems());
    run_check(Probe::Store(healthy.clone()), None, None, quiet.clone())
        .await
        .unwrap();
    assert_eq!(quiet.list(), Vec::new());
    assert_eq!(healthy.ids(), Vec::<String>::new());
}

#[tokio::test]
async fn shutdown_does_not_wait_for_ever_for_a_sweep_the_keyring_holds_up() {
    let (release, held) = std::sync::mpsc::channel::<()>();
    let sweep = tokio::task::spawn_blocking(move || {
        let _ = held.recv();
    });
    let end = finish_sweep(sweep, std::time::Duration::from_millis(50)).await;
    assert_eq!(end, SweepEnd::StillRunning);
    drop(release);
}

#[tokio::test]
async fn shutdown_reports_a_sweep_that_panicked_and_one_that_finished() {
    let panicked = tokio::task::spawn_blocking(|| panic!("keyring exploded"));
    assert_eq!(
        finish_sweep(panicked, std::time::Duration::from_secs(20)).await,
        SweepEnd::Panicked
    );
    let finished = tokio::task::spawn_blocking(|| {});
    assert_eq!(
        finish_sweep(finished, std::time::Duration::from_secs(20)).await,
        SweepEnd::Finished
    );
}

#[tokio::test]
async fn a_file_vault_that_takes_no_write_raises_the_vault_problem_and_a_skipped_check_nothing() {
    let broken = Arc::new(MemoryStore::new());
    broken.break_it();
    let list = Arc::new(problems());
    run_check(Probe::File(broken), None, None, list.clone())
        .await
        .unwrap();
    let raised = list.list();
    assert_eq!(raised.len(), 1, "{raised:?}");
    assert!(raised[0].title.contains("file vault does not take a write"));

    let quiet = Arc::new(problems());
    run_check(Probe::Skip, None, None, quiet.clone())
        .await
        .unwrap();
    assert_eq!(quiet.list(), Vec::new());
}

#[tokio::test]
async fn the_warning_path_runs_for_a_start_that_deserves_it_and_raises_no_problem_for_a_working_store()
 {
    let healthy = Arc::new(MemoryStore::new());
    let list = Arc::new(problems());
    run_check(
        Probe::Store(healthy),
        None,
        Some(LoginSession::NotGraphical),
        list.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        list.list(),
        Vec::new(),
        "a warning is a log line, not a problem"
    );
}
