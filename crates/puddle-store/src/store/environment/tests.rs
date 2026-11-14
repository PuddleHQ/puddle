// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_types::EventSink;

use super::*;
use crate::clock::ManualClock;
use crate::store::Limits;

fn store() -> Store {
    owned(Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap())
}

/// `store` with the workspaces these tests use, each the owner's.
fn owned(store: Store) -> Store {
    for name in [
        "a", "b", "big", "empty", "full", "held", "other", "own", "shop",
    ] {
        crate::testing::own(&store, &ws(name));
    }
    store
}

fn name(text: &str) -> EnvName {
    EnvName::new(text).unwrap()
}

fn ws(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

fn plain(value: &str) -> EnvDraft {
    EnvDraft::plain(value).unwrap()
}

fn secret(id: &str, hosts: &[&str]) -> EnvDraft {
    EnvDraft::secret(
        StoredId::new(id).unwrap(),
        hosts.iter().map(|h| SecretHost::new(h).unwrap()).collect(),
    )
    .unwrap()
}

fn mint(name: &EnvName) -> String {
    format!("stand-in-for-{name}-{:04}", rand_counter())
}

fn rand_counter() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn start(store: &Store, workspace: &str) -> Vec<StartVar> {
    store
        .env_for_start(&ws(workspace), &mut |n| Ok(mint(n)))
        .unwrap()
}

#[test]
fn a_variable_is_set_listed_replaced_and_removed_per_scope() {
    let store = store();
    let global = EnvScope::Global;
    let shop = EnvScope::Workspace(ws("shop"));
    let made = store
        .set_env(UserId::OWNER, &global, &name("EDITOR"), plain("vim"))
        .unwrap();
    assert_eq!(made.replaced, None);
    assert_eq!(made.entry.value, EnvValue::Plain("vim".into()));
    assert_eq!(made.entry.scope, EnvScope::Global);
    store
        .set_env(UserId::OWNER, &shop, &name("EDITOR"), plain("nano"))
        .unwrap();
    store
        .set_env(UserId::OWNER, &shop, &name("A"), plain("1"))
        .unwrap();

    let second = store
        .set_env(UserId::OWNER, &global, &name("EDITOR"), plain("emacs"))
        .unwrap();
    assert_eq!(
        second.replaced.unwrap().value,
        EnvValue::Plain("vim".into())
    );
    assert_eq!(second.entry.id, made.entry.id, "the row stays");
    let names = |scope: &EnvScope| -> Vec<(String, EnvValue)> {
        store
            .env_entries(UserId::OWNER, scope)
            .unwrap()
            .into_iter()
            .map(|e| (e.name.to_string(), e.value))
            .collect()
    };
    assert_eq!(
        names(&global),
        [("EDITOR".into(), EnvValue::Plain("emacs".into()))]
    );
    assert_eq!(
        names(&shop),
        [
            ("A".into(), EnvValue::Plain("1".into())),
            ("EDITOR".into(), EnvValue::Plain("nano".into()))
        ]
    );
    assert_eq!(
        store
            .env_entry(UserId::OWNER, &shop, &name("A"))
            .unwrap()
            .unwrap()
            .value,
        EnvValue::Plain("1".into())
    );
    assert!(
        store
            .env_entry(UserId::OWNER, &shop, &name("B"))
            .unwrap()
            .is_none()
    );

    let gone = store.delete_env(UserId::OWNER, &shop, &name("A")).unwrap();
    assert_eq!(gone.name, name("A"));
    assert!(matches!(
        store.delete_env(UserId::OWNER, &shop, &name("A")),
        Err(StoreError::UnknownEnv { .. })
    ));
    let text = store
        .delete_env(UserId::OWNER, &shop, &name("A"))
        .unwrap_err()
        .to_string();
    assert_eq!(text, "no variable A in shop's environment");
}

#[test]
fn a_workspace_sees_its_own_variables_and_the_global_ones_it_does_not_hide() {
    let store = store();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("EDITOR"),
            plain("vim"),
        )
        .unwrap();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("LANG_X"),
            plain("en"),
        )
        .unwrap();
    let shop = ws("shop");
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Workspace(shop.clone()),
            &name("EDITOR"),
            plain("nano"),
        )
        .unwrap();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Workspace(ws("other")),
            &name("ZZ"),
            plain("not mine"),
        )
        .unwrap();
    let rows: Vec<(String, bool, bool)> = store
        .workspace_env(&shop)
        .unwrap()
        .into_iter()
        .map(|r| {
            (
                r.entry.name.to_string(),
                r.entry.scope == EnvScope::Global,
                r.overridden,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("EDITOR".into(), false, false),
            ("EDITOR".into(), true, true),
            ("LANG_X".into(), true, false),
        ]
    );
}

#[test]
fn a_secret_keeps_its_id_and_hosts_and_never_a_value() {
    let store = store();
    let scope = EnvScope::Workspace(ws("shop"));
    let made = store
        .set_env(
            UserId::OWNER,
            &scope,
            &name("NPM_TOKEN"),
            secret("env-1", &["registry.npmjs.org"]),
        )
        .unwrap();
    let kept = made.entry.value.as_secret().unwrap();
    assert_eq!(kept.id.as_str(), "env-1");
    assert_eq!(kept.hosts[0].as_str(), "registry.npmjs.org");
    // The database holds the reference and the hosts, no value column filled.
    let conn = lock(&store.conn);
    let (value, secret_id): (Option<String>, Option<String>) = conn
        .query_row("SELECT value, secret_id FROM env_vars", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((value, secret_id.as_deref()), (None, Some("env-1")));
}

#[test]
fn a_secret_keeps_its_place_changes_it_and_a_workspace_starts_with_it() {
    let store = store();
    let scope = EnvScope::Workspace(ws("shop"));
    let draft = |place: SecretPlace| {
        EnvDraft::secret_in(
            StoredId::new("env-1").unwrap(),
            vec![SecretHost::new("api.openweathermap.org").unwrap()],
            place,
        )
        .unwrap()
    };
    let appid = SecretPlace::Query(puddle_types::QueryName::new("appid").unwrap());
    let made = store
        .set_env(
            UserId::OWNER,
            &scope,
            &name("OWM_KEY"),
            draft(appid.clone()),
        )
        .unwrap();
    assert_eq!(made.entry.value.as_secret().unwrap().place, appid);
    assert!(matches!(
        &start(&store, "shop")[0].value,
        StartValue::Secret { place, .. } if *place == appid
    ));
    let column = |store: &Store| -> Option<String> {
        lock(&store.conn)
            .query_row("SELECT place FROM env_vars", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(column(&store).as_deref(), Some("query:appid"));

    for place in [SecretPlace::Path, SecretPlace::Header] {
        store
            .set_env(
                UserId::OWNER,
                &scope,
                &name("OWM_KEY"),
                draft(place.clone()),
            )
            .unwrap();
        let entry = store
            .env_entry(UserId::OWNER, &scope, &name("OWM_KEY"))
            .unwrap()
            .unwrap();
        assert_eq!(entry.value.as_secret().unwrap().place, place);
    }
    assert_eq!(column(&store), None, "header values: no place stored");

    // A secret that becomes plain loses its place; the database refuses a plain one with one.
    store
        .set_env(
            UserId::OWNER,
            &scope,
            &name("OWM_KEY"),
            draft(SecretPlace::Path),
        )
        .unwrap();
    store
        .set_env(UserId::OWNER, &scope, &name("OWM_KEY"), plain("x"))
        .unwrap();
    assert_eq!(column(&store), None);
    let refused = lock(&store.conn).execute("UPDATE env_vars SET place = 'path'", []);
    assert!(refused.is_err(), "a plain variable has no place");
}

#[test]
fn a_damaged_place_is_reported_as_corrupt() {
    let store = store();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("T"),
            secret("env-1", &["api.example.com"]),
        )
        .unwrap();
    lock(&store.conn)
        .execute("UPDATE env_vars SET place = 'query:a&b'", [])
        .unwrap();
    let err = store
        .env_entries(UserId::OWNER, &EnvScope::Global)
        .unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::Corrupt {
                table: "env_vars",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn a_stand_in_is_made_once_per_workspace_and_secret_and_kept() {
    let store = store();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("TOKEN"),
            secret("env-g", &["api.example.com"]),
        )
        .unwrap();
    let first = start(&store, "a");
    let again = start(&store, "a");
    assert_eq!(first, again, "the same stand-in at every start");
    let other = start(&store, "b");
    let text = |vars: &[StartVar]| vars[0].value.stand_in().unwrap().to_owned();
    assert_ne!(text(&first), text(&other), "each workspace has its own");

    // A new value or new hosts under the same row keep the stand-in.
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("TOKEN"),
            secret("env-g", &["other.example.com"]),
        )
        .unwrap();
    let moved = start(&store, "a");
    assert_eq!(text(&moved), text(&first));
    assert!(matches!(
        &moved[0].value,
        StartValue::Secret { hosts, .. } if hosts[0].as_str() == "other.example.com"
    ));

    // Removing the secret and making it again is a new secret with a new stand-in.
    store
        .delete_env(UserId::OWNER, &EnvScope::Global, &name("TOKEN"))
        .unwrap();
    assert_eq!(start(&store, "a"), []);
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("TOKEN"),
            secret("env-g2", &["api.example.com"]),
        )
        .unwrap();
    assert_ne!(text(&start(&store, "a")), text(&first));
}

#[test]
fn a_secret_that_becomes_plain_loses_its_stand_ins() {
    let store = store();
    let scope = EnvScope::Workspace(ws("a"));
    store
        .set_env(
            UserId::OWNER,
            &scope,
            &name("T"),
            secret("env-1", &["x.example.com"]),
        )
        .unwrap();
    start(&store, "a");
    let n = |sql: &str| -> i64 { lock(&store.conn).query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(n("SELECT count(*) FROM env_stand_ins"), 1);
    let change = store
        .set_env(UserId::OWNER, &scope, &name("T"), plain("now plain"))
        .unwrap();
    assert!(matches!(
        change.replaced.unwrap().value,
        EnvValue::Secret(_)
    ));
    assert_eq!(n("SELECT count(*) FROM env_stand_ins"), 0);
    assert_eq!(
        start(&store, "a")[0].value,
        StartValue::Plain("now plain".into())
    );
}

#[test]
fn the_workspace_entry_wins_over_the_global_one_at_start_and_only_the_winner_has_a_stand_in() {
    let store = store();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("T"),
            secret("env-g", &["g.example.com"]),
        )
        .unwrap();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Workspace(ws("a")),
            &name("T"),
            plain("mine"),
        )
        .unwrap();
    let vars = start(&store, "a");
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0].value, StartValue::Plain("mine".into()));
    assert_eq!(
        vars[0].value.stand_in(),
        None,
        "a plain variable has no stand-in"
    );
    let held: i64 = lock(&store.conn)
        .query_row("SELECT count(*) FROM env_stand_ins", [], |r| r.get(0))
        .unwrap();
    assert_eq!(held, 0, "the hidden global secret got none");
    // Another workspace still gets the global one.
    assert!(matches!(
        start(&store, "b")[0].value,
        StartValue::Secret { .. }
    ));
}

#[test]
fn a_stand_in_that_cannot_be_made_stops_the_start_with_its_reason() {
    let store = store();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("T"),
            secret("env-g", &["g.example.com"]),
        )
        .unwrap();
    let err = store
        .env_for_start(&ws("a"), &mut |_| Err("no randomness".into()))
        .unwrap_err();
    assert_eq!(err.to_string(), "no randomness");
    // Nothing half-made was kept.
    assert!(matches!(
        start(&store, "a")[0].value,
        StartValue::Secret { .. }
    ));
}

#[test]
fn a_scope_holds_at_most_256_variables_and_256_kib() {
    let store = store();
    let scope = EnvScope::Workspace(ws("a"));
    for i in 0..MAX_ENTRIES_PER_SCOPE {
        store
            .set_env(UserId::OWNER, &scope, &name(&format!("V{i}")), plain("x"))
            .unwrap();
    }
    let err = store
        .set_env(UserId::OWNER, &scope, &name("ONE_MORE"), plain("x"))
        .unwrap_err();
    assert!(err.to_string().contains("256 variables"), "{err}");
    // Replacing one is not adding one.
    store
        .set_env(UserId::OWNER, &scope, &name("V0"), plain("y"))
        .unwrap();
    // Another scope is counted on its own.
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("ONE_MORE"),
            plain("x"),
        )
        .unwrap();

    let big = EnvScope::Workspace(ws("big"));
    let chunk = "x".repeat(crate::environment::MAX_PLAIN_VALUE_BYTES);
    let fits = MAX_SCOPE_BYTES / (chunk.len() + 2);
    for i in 0..fits {
        store
            .set_env(
                UserId::OWNER,
                &big,
                &name(&format!("B{i:x}")),
                plain(&chunk),
            )
            .unwrap();
    }
    let err = store
        .set_env(UserId::OWNER, &big, &name("BIG"), plain(&chunk))
        .unwrap_err();
    assert!(err.to_string().contains("256 KiB"), "{err}");
    // A shorter value of an existing name that frees room is accepted.
    store
        .set_env(UserId::OWNER, &big, &name("B0"), plain(""))
        .unwrap();
    store
        .set_env(UserId::OWNER, &big, &name("BIG"), plain(&chunk))
        .unwrap();
}

#[test]
fn changes_say_which_scope_changed() {
    #[derive(Default)]
    struct Events(std::sync::Mutex<Vec<Event>>);
    impl EventSink for Events {
        fn emit(&self, event: Event) {
            self.0.lock().unwrap().push(event);
        }
    }
    let events = Arc::new(Events::default());
    let store = owned(
        Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default())
            .unwrap()
            .with_events(events.clone()),
    );
    store
        .set_env(UserId::OWNER, &EnvScope::Global, &name("A"), plain("1"))
        .unwrap();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Workspace(ws("shop")),
            &name("A"),
            plain("1"),
        )
        .unwrap();
    store
        .delete_env(UserId::OWNER, &EnvScope::Workspace(ws("shop")), &name("A"))
        .unwrap();
    store
        .delete_env(UserId::OWNER, &EnvScope::Global, &name("A"))
        .unwrap();
    assert_eq!(
        *events.0.lock().unwrap(),
        [
            Event::GlobalEnvChanged {},
            Event::WorkspaceEnvChanged {
                workspace: ws("shop")
            },
            Event::WorkspaceEnvChanged {
                workspace: ws("shop")
            },
            Event::GlobalEnvChanged {},
        ]
    );
}

#[test]
fn deleting_a_workspace_removes_its_variables_and_every_stand_in_it_held() {
    let store = store();
    let shop = ws("shop");
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("G"),
            secret("env-g", &["g.example.com"]),
        )
        .unwrap();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Workspace(shop.clone()),
            &name("W"),
            secret("env-w", &["w.example.com"]),
        )
        .unwrap();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Workspace(shop.clone()),
            &name("P"),
            plain("p"),
        )
        .unwrap();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Workspace(ws("other")),
            &name("W"),
            secret("env-o", &["w.example.com"]),
        )
        .unwrap();
    start(&store, "shop");
    start(&store, "other");

    let deletion = store.delete_workspace(&shop).unwrap();
    assert_eq!(
        deletion
            .secrets
            .iter()
            .map(|(name, id)| (name.as_str(), id.as_str()))
            .collect::<Vec<_>>(),
        [("W", "env-w")],
        "only the workspace's own secrets: the global one still serves the others"
    );
    assert_eq!(
        store
            .env_entries(UserId::OWNER, &EnvScope::Workspace(shop))
            .unwrap(),
        []
    );
    assert_eq!(
        store
            .env_entries(UserId::OWNER, &EnvScope::Global)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .env_entries(UserId::OWNER, &EnvScope::Workspace(ws("other")))
            .unwrap()
            .len(),
        1
    );
    let held: Vec<String> = {
        let conn = lock(&store.conn);
        let mut stmt = conn
            .prepare("SELECT workspace_id FROM env_stand_ins ORDER BY 1")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(held, ["other", "other"], "other's two stand-ins stay");
}

#[test]
fn a_scope_may_hold_exactly_256_kib_and_not_a_byte_more() {
    let store = store();
    let scope = EnvScope::Workspace(ws("full"));
    let chunk = "x".repeat(crate::environment::MAX_PLAIN_VALUE_BYTES);
    // Names of two bytes (`B0`..) with a full value: each takes `len + 2`.
    let fits = MAX_SCOPE_BYTES / (chunk.len() + 2);
    for i in 0..fits {
        store
            .set_env(
                UserId::OWNER,
                &scope,
                &name(&format!("B{i:x}")),
                plain(&chunk),
            )
            .unwrap();
    }
    let left = MAX_SCOPE_BYTES - fits * (chunk.len() + 2);
    let exact = "y".repeat(left - 2);
    let over = "y".repeat(left - 1);
    let err = store
        .set_env(UserId::OWNER, &scope, &name("EX"), plain(&over))
        .unwrap_err();
    assert!(err.to_string().contains("256 KiB"), "{err}");
    store
        .set_env(UserId::OWNER, &scope, &name("EX"), plain(&exact))
        .unwrap();
}

#[test]
fn deleting_a_workspace_says_its_environment_changed_only_when_it_held_something() {
    #[derive(Default)]
    struct Events(std::sync::Mutex<Vec<Event>>);
    impl EventSink for Events {
        fn emit(&self, event: Event) {
            self.0.lock().unwrap().push(event);
        }
    }
    let events = Arc::new(Events::default());
    let store = owned(
        Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default())
            .unwrap()
            .with_events(events.clone()),
    );
    let said = |workspace: &str| -> usize {
        let mut seen = events.0.lock().unwrap();
        let n = seen
            .iter()
            .filter(|e| {
                **e == Event::WorkspaceEnvChanged {
                    workspace: ws(workspace),
                }
            })
            .count();
        seen.clear();
        n
    };
    // Its own variable, no stand-in.
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Workspace(ws("own")),
            &name("A"),
            plain("1"),
        )
        .unwrap();
    events.0.lock().unwrap().clear();
    store.delete_workspace(&ws("own")).unwrap();
    assert_eq!(said("own"), 1);

    // No variable of its own, only a stand-in for a global secret.
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("G"),
            secret("env-g", &["g.example.com"]),
        )
        .unwrap();
    start(&store, "held");
    events.0.lock().unwrap().clear();
    store.delete_workspace(&ws("held")).unwrap();
    assert_eq!(said("held"), 1);

    // Nothing at all.
    store.delete_workspace(&ws("empty")).unwrap();
    assert_eq!(said("empty"), 0);
}

#[test]
fn a_row_edited_by_hand_to_something_that_does_not_parse_is_reported() {
    let store = store();
    let scope = EnvScope::Workspace(ws("a"));
    store
        .set_env(
            UserId::OWNER,
            &scope,
            &name("T"),
            secret("env-1", &["x.example.com"]),
        )
        .unwrap();
    lock(&store.conn)
        .execute("UPDATE env_vars SET hosts = 'not json'", [])
        .unwrap();
    assert!(matches!(
        store.env_entries(UserId::OWNER, &scope),
        Err(StoreError::Corrupt {
            table: "env_vars",
            ..
        })
    ));
    lock(&store.conn)
        .execute("UPDATE env_vars SET hosts = '[\"*.com\"]'", [])
        .unwrap();
    assert!(
        store.env_entries(UserId::OWNER, &scope).is_err(),
        "a host list that fails its own rules"
    );
    lock(&store.conn)
        .execute(
            "UPDATE env_vars SET hosts = '[]', secret_id = 'has space'",
            [],
        )
        .unwrap();
    assert!(
        store.env_entries(UserId::OWNER, &scope).is_err(),
        "an id the credential store would not take"
    );
    lock(&store.conn)
        .execute("UPDATE env_vars SET name = '1bad', secret_id = 'env-1'", [])
        .unwrap();
    assert!(
        store.env_entries(UserId::OWNER, &scope).is_err(),
        "a name that is not one"
    );
}

#[test]
fn the_ids_in_use_are_not_listed_when_a_secret_row_is_damaged() {
    let store = store();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name("TOKEN"),
            secret("env-1", &["a.example.com"]),
        )
        .unwrap();
    lock(&store.conn)
        .execute("UPDATE env_vars SET secret_id = 'has space'", [])
        .unwrap();
    // A caller that removes what is not listed must see an error, not a short list.
    assert!(matches!(
        store.secret_ids_in_use(),
        Err(StoreError::Corrupt {
            table: "env_vars",
            ..
        })
    ));
}

#[test]
fn a_row_of_the_wrong_shape_is_reported_even_if_the_table_checks_were_bypassed() {
    let store = store();
    let scope = EnvScope::Workspace(ws("a"));
    store
        .set_env(
            UserId::OWNER,
            &scope,
            &name("T"),
            secret("env-1", &["x.example.com"]),
        )
        .unwrap();
    // A plain variable that also has a secret's id: the table's own checks refuse it, so the
    // checks are switched off for the one edit.
    lock(&store.conn)
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON;
                 UPDATE env_vars SET kind = 'plain', value = 'x';
                 PRAGMA ignore_check_constraints = OFF;",
        )
        .unwrap();
    let err = store
        .env_entries(UserId::OWNER, &scope)
        .unwrap_err()
        .to_string();
    assert!(err.contains("wrong shape"), "{err}");
}

#[test]
fn a_name_a_later_release_reserves_stays_listable_and_removable() {
    let store = store();
    let scope = EnvScope::Workspace(ws("a"));
    store
        .set_env(UserId::OWNER, &scope, &name("T"), plain("v"))
        .unwrap();
    lock(&store.conn)
        .execute("UPDATE env_vars SET name = 'HTTPS_PROXY'", [])
        .unwrap();
    let listed = store.env_entries(UserId::OWNER, &scope).unwrap();
    assert_eq!(listed[0].name.as_str(), "HTTPS_PROXY");
    let proxy = EnvName::existing("HTTPS_PROXY").unwrap();
    store.delete_env(UserId::OWNER, &scope, &proxy).unwrap();
    assert_eq!(store.env_entries(UserId::OWNER, &scope).unwrap(), []);
}
