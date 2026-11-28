// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rule sets and System managed (`docs/spec/rules.md` §7, R-36 to R-43), through the store's
//! public API.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] functions fail the test by panicking"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use puddle_store::testing::own;
use puddle_store::{
    Actor, BUILT_IN_SETS, Effect, Limits, ManualClock, NewRule, Pattern, PendingState, Precedence,
    Resolution, Scope, ScopeChoice, Store, StoreError, SystemPlan, SystemReason, parse_rule_set,
};
use puddle_types::{
    ConnectionDecision, ConnectionEvent, ConnectionLog, Decision, EgressRequest, Fill, Host,
    PatternKind, PendingId, PendingOutcome, Rgb, RuleSetId, SuffixAllows, WorkspaceId,
};
use serde_json::Value;

const OWNER: puddle_store::UserId = puddle_store::UserId::OWNER;
const T0: u64 = 1_800_000_000_000;

fn fixture() -> (Arc<ManualClock>, Store) {
    let clock = Arc::new(ManualClock::new(T0));
    let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
    own_workspaces(&store);
    (clock, store)
}

/// The workspaces these tests use, each the owner's.
fn own_workspaces(store: &Store) {
    for name in ["a", "b", "c", "gone", "ssh"] {
        own(store, &sb(name));
    }
}

fn sb(id: &str) -> WorkspaceId {
    WorkspaceId::new(id).unwrap()
}

fn req(workspace: &str, host: &str) -> EgressRequest {
    EgressRequest::new(sb(workspace), Host::parse_normalised(host).unwrap(), 443)
}

fn decide(store: &Store, workspace: &str, host: &str) -> Decision {
    store
        .decide(&req(workspace, host), SuffixAllows::Count)
        .unwrap()
}

fn new_rule(scope: Scope, pattern: &str, effect: Effect) -> NewRule {
    NewRule {
        scope,
        pattern: Pattern::parse(pattern).unwrap(),
        effect,
        expires_at: None,
        created_by: Actor::Ui,
    }
}

fn pending(decision: Decision) -> PendingId {
    match decision {
        Decision::Pending(PendingOutcome::New(id) | PendingOutcome::Repeat(id)) => id,
        other => panic!("expected pending, got {other:?}"),
    }
}

fn audit_of(store: &Store, kind: &str) -> Vec<Value> {
    store
        .audit_lines(0, 100_000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| serde_json::from_str::<Value>(&line).unwrap())
        .filter(|v| v["type"] == kind)
        .collect()
}

fn user_set(store: &Store, name: &str) -> i64 {
    match store
        .create_rule_set(OWNER, name, "", None, Actor::Ui)
        .unwrap()
        .id
    {
        RuleSetId::User(id) => id,
        other => panic!("{other:?}"),
    }
}

fn plan(everywhere: &[SystemReason], per: &[(&str, &[SystemReason])]) -> SystemPlan {
    SystemPlan {
        everywhere: everywhere.iter().copied().collect(),
        workspaces: per
            .iter()
            .map(|(s, r)| (sb(s), r.iter().copied().collect::<BTreeSet<_>>()))
            .collect::<BTreeMap<_, _>>(),
        ..SystemPlan::default()
    }
}

#[test]
fn r36_built_in_sets_are_listed_read_only_and_ship_off() {
    let (_, store) = fixture();
    let sets = store.rule_sets(OWNER).unwrap();
    assert_eq!(sets.len(), BUILT_IN_SETS.len());
    for (set, shipped) in sets.iter().zip(BUILT_IN_SETS) {
        assert_eq!(set.id, RuleSetId::BuiltIn(shipped.slug));
        assert!(!set.on_by_default() && !set.is_on(&sb("a")));
        assert!(
            set.entries
                .iter()
                .all(|e| e.effect == Effect::Allow && e.rule_id.is_none())
        );
        assert_eq!(set.changed_at, None);
    }
    // A fresh install decides nothing: github.com is in a built-in set that is off.
    assert!(matches!(
        decide(&store, "a", "github.com"),
        Decision::Pending(_)
    ));
    // A built-in set has no rule rows to delete or rename.
    assert!(matches!(
        store.update_rule_set(OWNER, 1, "x", "", None, Actor::Ui),
        Err(StoreError::UnknownRuleSet(_))
    ));
}

#[test]
fn r36_an_update_that_changes_a_built_in_set_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let clock = Arc::new(ManualClock::new(T0));
    drop(Store::open(&path, clock.clone(), Limits::default()).unwrap());
    // Pretend the previous version shipped GitHub with one other host and without api.github.com.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE builtin_sets_seen SET entries = ?1 WHERE slug = 'github'",
        [r#"["github.com","old.github.com"]"#],
    )
    .unwrap();
    drop(conn);
    clock.advance(5_000);
    let store = Store::open(&path, clock.clone(), Limits::default()).unwrap();
    let changed = audit_of(&store, "rule_set_changed");
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0]["set_id"], "builtin:github");
    assert_eq!(changed[0]["removed"], serde_json::json!(["old.github.com"]));
    assert!(
        changed[0]["added"]
            .as_array()
            .unwrap()
            .contains(&Value::from("api.github.com"))
    );
    let github = store.rule_set(OWNER, RuleSetId::BuiltIn("github")).unwrap();
    assert_eq!(github.changed_at, Some(T0 + 5_000));
    // Opening again with the same catalogue records nothing new.
    drop(store);
    let store = Store::open(&path, clock, Limits::default()).unwrap();
    assert_eq!(audit_of(&store, "rule_set_changed").len(), 1);
}

#[test]
fn r37_switches_apply_globally_with_a_per_workspace_override_and_close_waiting_requests() {
    let (_, store) = fixture();
    let github = RuleSetId::BuiltIn("github");
    let waiting_a = pending(decide(&store, "a", "api.github.com"));
    let waiting_b = pending(decide(&store, "b", "api.github.com"));
    // Off for b first, then on everywhere: only a's request closes.
    store
        .switch_rule_set(OWNER, github, Some(&sb("b")), Some(false), Actor::Ui)
        .unwrap();
    let closed = store
        .switch_rule_set(OWNER, github, None, Some(true), Actor::Ui)
        .unwrap();
    assert_eq!(closed, vec![waiting_a]);
    let row = store.pending(waiting_a).unwrap();
    assert_eq!(row.state, PendingState::Allowed);
    assert_eq!(row.rule_id, None);
    assert_eq!(row.rule_set.as_deref(), Some("builtin:github"));
    assert_eq!(
        store.pending(waiting_b).unwrap().state,
        PendingState::Requested
    );
    assert_eq!(
        decide(&store, "a", "api.github.com"),
        Decision::SetAllow {
            set: github,
            rule_id: None,
            pattern: PatternKind::Exact
        }
    );
    assert!(matches!(
        decide(&store, "b", "api.github.com"),
        Decision::Pending(_)
    ));
    // b back to following the global switch.
    let closed = store
        .switch_rule_set(OWNER, github, Some(&sb("b")), None, Actor::Ui)
        .unwrap();
    assert_eq!(closed, vec![waiting_b]);
    let info = store.rule_set(OWNER, github).unwrap();
    assert_eq!((info.global, info.overrides), (Some(true), vec![]));
    let switched = audit_of(&store, "rule_set_switched");
    assert_eq!(switched.len(), 3);
    assert_eq!(switched[1]["workspace_id"], Value::Null);
    assert_eq!(switched[1]["enabled"], true);
    assert_eq!(switched[2]["enabled"], Value::Null);
    assert_eq!(switched[2]["actor"], "ui");
    // Off again: the next request goes pending (R-8 applies).
    store
        .switch_rule_set(OWNER, github, None, Some(false), Actor::Ui)
        .unwrap();
    assert!(matches!(
        decide(&store, "a", "api.github.com"),
        Decision::Pending(_)
    ));
}

#[test]
fn r37_only_real_sets_switch_and_only_users_switch_them() {
    let (_, store) = fixture();
    assert!(matches!(
        store.switch_rule_set(OWNER, RuleSetId::System, None, Some(false), Actor::Ui),
        Err(StoreError::NotSwitchable)
    ));
    assert!(matches!(
        store.switch_rule_set(OWNER, RuleSetId::User(42), None, Some(true), Actor::Ui),
        Err(StoreError::UnknownRuleSet(id)) if id == "user:42"
    ));
    assert!(matches!(
        store.switch_rule_set(
            OWNER,
            RuleSetId::BuiltIn("nope"),
            None,
            Some(true),
            Actor::Ui
        ),
        Err(StoreError::UnknownRuleSet(_))
    ));
    assert!(matches!(
        store.switch_rule_set(
            OWNER,
            RuleSetId::BuiltIn("github"),
            None,
            Some(true),
            Actor::System
        ),
        Err(StoreError::SystemActor)
    ));
}

#[test]
fn r38_sets_you_make_hold_rules_and_inbox_approvals() {
    let (_, store) = fixture();
    let azure = user_set(&store, "Azure work");
    let info = store.rule_set(OWNER, RuleSetId::User(azure)).unwrap();
    assert!(info.on_by_default() && info.entries.is_empty() && info.created_at == Some(T0));
    // An entry added directly.
    let entry = store
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&new_rule(Scope::Set(azure), "*.azure.com", Effect::Allow))
        .unwrap();
    assert!(matches!(
        decide(&store, "a", "portal.azure.com"),
        Decision::SetAllow { set: RuleSetId::User(id), rule_id: Some(r), pattern: PatternKind::Suffix }
            if id == azure && r == entry.id
    ));
    // Approve into the set from the inbox: the rule joins the set, and other workspaces' rows close.
    let in_a = pending(decide(&store, "a", "login.microsoftonline.com"));
    let in_b = pending(decide(&store, "b", "login.microsoftonline.com"));
    let mut into_set = Resolution::allow();
    into_set.scope = ScopeChoice::Set(azure);
    let decided = store
        .as_user(puddle_store::UserId::OWNER)
        .resolve_pending(in_a, &into_set, Actor::Ui)
        .unwrap();
    assert_eq!(decided.rule.scope, Scope::Set(azure));
    assert_eq!(decided.also_closed, vec![in_b]);
    assert_eq!(
        store
            .rule_set(OWNER, RuleSetId::User(azure))
            .unwrap()
            .entries
            .len(),
        2
    );
    // Not into a set that is off for the row's workspace: that would allow nothing.
    store
        .switch_rule_set(
            OWNER,
            RuleSetId::User(azure),
            Some(&sb("c")),
            Some(false),
            Actor::Ui,
        )
        .unwrap();
    let in_c = pending(decide(&store, "c", "graph.microsoft.com"));
    assert!(matches!(
        store.as_user(puddle_store::UserId::OWNER).resolve_pending(in_c, &into_set, Actor::Ui),
        Err(StoreError::RuleSetOff { set, workspace }) if set == format!("user:{azure}") && workspace == "c"
    ));
    assert_eq!(store.pending(in_c).unwrap().state, PendingState::Requested);
    // Nor into a set that doesn't exist.
    into_set.scope = ScopeChoice::Set(999);
    assert!(matches!(
        store
            .as_user(puddle_store::UserId::OWNER)
            .resolve_pending(in_c, &into_set, Actor::Ui),
        Err(StoreError::UnknownRuleSet(_))
    ));
    assert!(matches!(
        store
            .as_user(puddle_store::UserId::OWNER)
            .add_rule(&new_rule(Scope::Set(999), "x.example", Effect::Allow)),
        Err(StoreError::UnknownRuleSet(_))
    ));
}

#[test]
fn r38_set_names_are_checked_renamed_and_deleting_a_set_deletes_its_entries() {
    let (_, store) = fixture();
    let one = user_set(&store, "  Work  ");
    assert_eq!(
        store.rule_set(OWNER, RuleSetId::User(one)).unwrap().name,
        "Work"
    );
    for (name, why) in [
        ("", "a name is needed"),
        ("work", "another rule set has this name"),
        ("GitHub", "another rule set has this name"),
        ("bad\nname", "no control characters"),
    ] {
        assert!(
            matches!(store.create_rule_set(OWNER, name, "", None, Actor::Ui), Err(StoreError::RuleSetName(w)) if w == why),
            "{name:?}"
        );
    }
    assert!(matches!(
        store.create_rule_set(OWNER, &"x".repeat(65), "", None, Actor::Ui),
        Err(StoreError::RuleSetName(_))
    ));
    assert!(matches!(
        store.create_rule_set(OWNER, "ok", &"d".repeat(501), None, Actor::Ui),
        Err(StoreError::RuleSetName(_))
    ));
    assert!(matches!(
        store.create_rule_set(OWNER, "sys", "", None, Actor::System),
        Err(StoreError::SystemActor)
    ));
    let renamed = store
        .update_rule_set(
            OWNER,
            one,
            "Client X",
            "hosts for client X",
            None,
            Actor::Api,
        )
        .unwrap();
    assert_eq!(
        (renamed.name.as_str(), renamed.description.as_str()),
        ("Client X", "hosts for client X")
    );
    // Renaming to its own name in another case is fine.
    store
        .update_rule_set(OWNER, one, "client x", "", None, Actor::Api)
        .unwrap();
    let r = store
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&new_rule(Scope::Set(one), "x.example", Effect::Deny))
        .unwrap();
    store
        .switch_rule_set(OWNER, RuleSetId::User(one), None, Some(true), Actor::Ui)
        .unwrap();
    let deleted = store.delete_rule_set(OWNER, one, Actor::Ui).unwrap();
    assert_eq!(deleted.entries.len(), 1);
    assert!(
        store
            .rules_every_user(&puddle_store::testing::all_users())
            .iter()
            .all(|rule| rule.id != r.id)
    );
    assert!(matches!(
        decide(&store, "a", "x.example"),
        Decision::Pending(_)
    ));
    assert_eq!(audit_of(&store, "rule_set_deleted").len(), 1);
    let gone = audit_of(&store, "rule_deleted");
    assert_eq!(gone[0]["reason"], "set_deleted");
    assert_eq!(gone[0]["rule"]["scope"], "set");
    assert_eq!(gone[0]["rule"]["set_id"], one);
    assert_eq!(
        audit_of(&store, "rule_set_created")[0]["rule_set"]["name"],
        "Work"
    );
    assert_eq!(audit_of(&store, "rule_set_updated").len(), 2);
    assert!(matches!(
        store.delete_rule_set(OWNER, one, Actor::Ui),
        Err(StoreError::UnknownRuleSet(_))
    ));
    // The name is free again.
    user_set(&store, "client x");
}

#[test]
fn r39_your_own_rules_decide_before_any_set() {
    let (_, store) = fixture();
    store
        .set_system_managed(&plan(&[SystemReason::MicrosoftServer], &[]))
        .unwrap();
    // E1: your global deny of *.visualstudio.com beats System managed's exact allow.
    let deny = store
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&new_rule(Scope::Global, "*.visualstudio.com", Effect::Deny))
        .unwrap();
    assert_eq!(
        decide(&store, "a", "update.code.visualstudio.com"),
        Decision::Deny {
            rule_id: deny.id,
            pattern: PatternKind::Suffix
        }
    );
    assert!(decide(&store, "a", "vscode.download.prss.microsoft.com").is_allow());
    // E2: your allow of *.example.com; your set blocks ads.example.com; your exact allow wins.
    let blocks = user_set(&store, "Trackers");
    store
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&new_rule(Scope::Global, "*.example.com", Effect::Allow))
        .unwrap();
    let block = store
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&new_rule(
            Scope::Set(blocks),
            "ads.example.com",
            Effect::Deny,
        ))
        .unwrap();
    assert_eq!(
        decide(&store, "a", "ads.example.com"),
        Decision::SetDeny {
            set: RuleSetId::User(blocks),
            rule_id: Some(block.id),
            pattern: PatternKind::Exact
        }
    );
    let mine = store
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&new_rule(
            Scope::Workspace(sb("a")),
            "ads.example.com",
            Effect::Allow,
        ))
        .unwrap();
    assert_eq!(
        decide(&store, "a", "ads.example.com"),
        Decision::Allow {
            rule_id: mine.id,
            pattern: PatternKind::Exact
        }
    );
    assert!(matches!(
        decide(&store, "b", "ads.example.com"),
        Decision::SetDeny { .. }
    ));
}

#[test]
fn r38_deleting_one_entry_of_a_set_that_is_on_makes_that_host_pending_everywhere_it_is_on() {
    let (_, store) = fixture();
    let set = user_set(&store, "Two hosts");
    let entry = |pattern: &str| {
        store
            .as_user(OWNER)
            .add_rule(&new_rule(Scope::Set(set), pattern, Effect::Allow))
            .unwrap()
    };
    let gone = entry("gone.example");
    let kept = entry("kept.example");
    let id = RuleSetId::User(set);
    // On for a and b (the default of a set of your own), off for c.
    store
        .switch_rule_set(OWNER, id, Some(&sb("c")), Some(false), Actor::Ui)
        .unwrap();
    for ws in ["a", "b"] {
        for host in ["gone.example", "kept.example"] {
            assert!(
                matches!(decide(&store, ws, host), Decision::SetAllow { .. }),
                "{ws} {host}"
            );
        }
    }
    store.delete_rule(gone.id, Actor::Ui).unwrap();
    for ws in ["a", "b"] {
        assert!(
            matches!(decide(&store, ws, "gone.example"), Decision::Pending(_)),
            "{ws}: the deleted entry no longer allows"
        );
        assert_eq!(
            decide(&store, ws, "kept.example"),
            Decision::SetAllow {
                set: id,
                rule_id: Some(kept.id),
                pattern: PatternKind::Exact
            },
            "{ws}: the other entry still allows"
        );
    }
    assert!(matches!(
        decide(&store, "c", "kept.example"),
        Decision::Pending(_)
    ));
    assert_eq!(store.rule_set(OWNER, id).unwrap().entries.len(), 1);
}

#[test]
fn r37_switching_package_registries_on_for_one_workspace_allows_its_hosts_there_only() {
    let (_, store) = fixture();
    let registries = RuleSetId::BuiltIn("package-registries");
    let hosts = [
        "pypi.org",
        "files.pythonhosted.org",
        "crates.io",
        "static.crates.io",
        "index.crates.io",
        "registry.npmjs.org",
        "repo.maven.apache.org",
    ];
    for host in hosts {
        assert!(
            matches!(decide(&store, "b", host), Decision::Pending(_)),
            "{host} asks first while the set is off"
        );
    }
    store
        .switch_rule_set(OWNER, registries, Some(&sb("b")), Some(true), Actor::Ui)
        .unwrap();
    for host in hosts {
        assert_eq!(
            decide(&store, "b", host),
            Decision::SetAllow {
                set: registries,
                rule_id: None,
                pattern: PatternKind::Exact
            },
            "{host} in b"
        );
        assert!(
            matches!(decide(&store, "a", host), Decision::Pending(_)),
            "{host} still asks in a"
        );
    }
}

#[test]
fn r39_a_set_may_hold_allows_and_denies_and_the_most_specific_entry_wins() {
    let (_, store) = fixture();
    let mixed = user_set(&store, "Mixed");
    let entry = |pattern: &str, effect| {
        store
            .as_user(puddle_store::UserId::OWNER)
            .add_rule(&new_rule(Scope::Set(mixed), pattern, effect))
            .unwrap()
    };
    let allow = entry("*.example.com", Effect::Allow);
    let deny = entry("ads.example.com", Effect::Deny);
    entry("*.cdn.example.com", Effect::Allow);
    let tie_deny = entry("*.cdn.example.com", Effect::Deny);
    let set = RuleSetId::User(mixed);
    assert_eq!(
        decide(&store, "a", "www.example.com"),
        Decision::SetAllow {
            set,
            rule_id: Some(allow.id),
            pattern: PatternKind::Suffix
        }
    );
    assert_eq!(
        decide(&store, "a", "ads.example.com"),
        Decision::SetDeny {
            set,
            rule_id: Some(deny.id),
            pattern: PatternKind::Exact
        }
    );
    // Equal detail: the deny wins.
    assert_eq!(
        decide(&store, "a", "img.cdn.example.com"),
        Decision::SetDeny {
            set,
            rule_id: Some(tie_deny.id),
            pattern: PatternKind::Suffix
        }
    );
}

#[test]
fn r41_system_managed_follows_the_plan_and_records_each_change() {
    let (_, store) = fixture();
    let waiting = pending(decide(&store, "a", "open-vsx.org"));
    let closed = store
        .set_system_managed(&plan(&[SystemReason::CodeServer], &[]))
        .unwrap();
    assert_eq!(closed, vec![waiting]);
    let row = store.pending(waiting).unwrap();
    assert_eq!(
        (row.state, row.decided_by, row.rule_set.as_deref()),
        (PendingState::Allowed, Some(Actor::System), Some("system"))
    );
    assert!(matches!(
        decide(&store, "a", "openvsx.eclipsecontent.org"),
        Decision::SetAllow {
            set: RuleSetId::System,
            rule_id: None,
            ..
        }
    ));
    // Microsoft's hosts are not allowed for code-server.
    assert!(matches!(
        decide(&store, "a", "marketplace.visualstudio.com"),
        Decision::Pending(_)
    ));
    // Direct SSH on for one workspace: Microsoft's hosts there only.
    store
        .set_system_managed(&plan(
            &[SystemReason::CodeServer],
            &[("ssh", &[SystemReason::DirectSsh])],
        ))
        .unwrap();
    assert!(decide(&store, "ssh", "marketplace.visualstudio.com").is_allow());
    assert!(matches!(
        decide(&store, "a", "marketplace.visualstudio.com"),
        Decision::Pending(_)
    ));
    let hosts = store
        .system_managed_every_user(&puddle_store::testing::all_users())
        .unwrap();
    assert!(hosts.iter().any(|h| h.pattern == "open-vsx.org"
        && h.reason == SystemReason::CodeServer
        && h.workspace.is_none()));
    assert!(hosts.iter().any(|h| h.pattern == "*.gallery.vsassets.io"
        && h.reason == SystemReason::DirectSsh
        && h.workspace == Some(sb("ssh"))));
    // The same plan again changes and records nothing.
    let before = audit_of(&store, "system_managed_changed").len();
    assert_eq!(before, 2);
    assert_eq!(
        store
            .set_system_managed(&plan(
                &[SystemReason::CodeServer],
                &[("ssh", &[SystemReason::DirectSsh])],
            ))
            .unwrap(),
        Vec::<PendingId>::new()
    );
    assert_eq!(audit_of(&store, "system_managed_changed").len(), before);
    // Switching to Microsoft's server: Open VSX goes, the Marketplace comes, for everyone.
    store
        .set_system_managed(&plan(&[SystemReason::MicrosoftServer], &[]))
        .unwrap();
    let records = audit_of(&store, "system_managed_changed");
    let last_two: Vec<_> = records.iter().rev().take(2).collect();
    assert!(
        last_two.iter().any(
            |r| r["workspace_id"] == "ssh" && r["removed"] == serde_json::json!(["direct_ssh"])
        )
    );
    assert!(last_two.iter().any(|r| r["workspace_id"].is_null()
        && r["added"] == serde_json::json!(["microsoft_server"])
        && r["removed"] == serde_json::json!(["code_server"])));
    assert!(matches!(
        decide(&store, "a", "open-vsx.org"),
        Decision::Pending(_)
    ));
    assert!(decide(&store, "a", "marketplace.visualstudio.com").is_allow());
    // Nothing chosen: nothing allowed.
    store.set_system_managed(&SystemPlan::default()).unwrap();
    assert_eq!(
        store
            .system_managed_every_user(&puddle_store::testing::all_users())
            .unwrap(),
        Vec::new()
    );
    assert!(matches!(
        decide(&store, "a", "marketplace.visualstudio.com"),
        Decision::Pending(_)
    ));
}

#[test]
fn r41_deleting_a_workspace_removes_its_switches_and_system_reasons() {
    let (_, store) = fixture();
    store
        .set_system_managed(&plan(&[], &[("gone", &[SystemReason::DirectSsh])]))
        .unwrap();
    store
        .switch_rule_set(
            OWNER,
            RuleSetId::BuiltIn("github"),
            Some(&sb("gone")),
            Some(true),
            Actor::Ui,
        )
        .unwrap();
    store.delete_workspace(&sb("gone")).unwrap();
    assert_eq!(
        store
            .system_managed_every_user(&puddle_store::testing::all_users())
            .unwrap(),
        Vec::new()
    );
    let github = store.rule_set(OWNER, RuleSetId::BuiltIn("github")).unwrap();
    assert_eq!(github.overrides, Vec::new());
}

#[test]
fn r42_set_allows_need_an_exact_rule_of_your_own_for_local_destinations() {
    let (_, store) = fixture();
    store
        .switch_rule_set(
            OWNER,
            RuleSetId::BuiltIn("github"),
            None,
            Some(true),
            Actor::Ui,
        )
        .unwrap();
    // After a local address, the proxy asks again without wildcard-like allows: no match.
    let again = store
        .decide(&req("a", "github.com"), SuffixAllows::Ignore)
        .unwrap();
    assert!(matches!(again, Decision::Pending(_)));
    // Your own exact allow does count.
    let mine = store
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&new_rule(Scope::Global, "github.com", Effect::Allow))
        .unwrap();
    assert_eq!(
        store
            .decide(&req("a", "github.com"), SuffixAllows::Ignore)
            .unwrap(),
        Decision::Allow {
            rule_id: mine.id,
            pattern: PatternKind::Exact
        }
    );
}

#[test]
fn r14_switching_sets_never_closes_a_row_that_needs_an_exact_allow() {
    let (_, store) = fixture();
    let corp = store
        .as_user(OWNER)
        .add_rule(&new_rule(Scope::Global, ".corp.example", Effect::Allow))
        .unwrap();
    // The proxy found a local address behind the name and asked again without suffix allows.
    let local = req("a", "db.corp.example");
    let row = pending(store.decide(&local, SuffixAllows::Ignore).unwrap());
    let github_row = pending(
        store
            .decide(&req("a", "api.github.com"), SuffixAllows::Ignore)
            .unwrap(),
    );
    // An unrelated set switch, a set that matches switched on, one of its entries switched off
    // and on, and a System managed change: each looks again at the waiting rows.
    let mut closed = store
        .switch_rule_set(
            OWNER,
            RuleSetId::BuiltIn("debian-ubuntu"),
            None,
            Some(false),
            Actor::Ui,
        )
        .unwrap();
    closed.extend(
        store
            .switch_rule_set(
                OWNER,
                RuleSetId::BuiltIn("github"),
                None,
                Some(true),
                Actor::Ui,
            )
            .unwrap(),
    );
    for enabled in [false, true] {
        closed.extend(
            store
                .switch_builtin_entry(
                    OWNER,
                    RuleSetId::BuiltIn("github"),
                    "api.github.com",
                    enabled,
                    Actor::Ui,
                )
                .unwrap(),
        );
    }
    closed.extend(
        store
            .set_system_managed(&plan(&[SystemReason::CodeServer], &[]))
            .unwrap(),
    );
    assert_eq!(closed, Vec::<PendingId>::new());
    for id in [row, github_row] {
        assert_eq!(store.pending(id).unwrap().state, PendingState::Requested);
    }
    assert_eq!(audit_of(&store, "pending_decided"), Vec::<Value>::new());
    // A request the suffix allow does count for still closes on a switch.
    let public = pending(decide(&store, "b", "deb.debian.org"));
    let closed = store
        .switch_rule_set(
            OWNER,
            RuleSetId::BuiltIn("debian-ubuntu"),
            None,
            Some(true),
            Actor::Ui,
        )
        .unwrap();
    assert_eq!(closed, vec![public]);
    assert_eq!(
        store.decide(&local, SuffixAllows::Count).unwrap(),
        Decision::Allow {
            rule_id: corp.id,
            pattern: PatternKind::Suffix
        }
    );
}

#[test]
fn r43_connection_records_name_the_set_that_decided() {
    let (_, store) = fixture();
    store
        .set_system_managed(&plan(&[SystemReason::CodeServer], &[]))
        .unwrap();
    let request = req("a", "open-vsx.org");
    let decision = decide(&store, "a", "open-vsx.org");
    let event = ConnectionEvent::decided(&request, &decision);
    store.record(&event);
    let record = audit_of(&store, "connection").pop().unwrap();
    assert_eq!(record["decision"], "allow");
    assert_eq!(record["reason"], "rule");
    assert_eq!(record["rule_id"], Value::Null);
    assert_eq!(record["rule_set"], "system");
    assert_eq!(event.decision, ConnectionDecision::Allow);
    let rows = store.rules_every_user(&puddle_store::testing::all_users());
    assert!(rows.is_empty(), "System managed writes no rule rows");
}

#[test]
fn r40_system_managed_lists_every_host_with_its_reason_and_no_rule_rows() {
    let (_, store) = fixture();
    store
        .set_system_managed(&plan(&[SystemReason::MicrosoftServer], &[]))
        .unwrap();
    let hosts = store
        .system_managed_every_user(&puddle_store::testing::all_users())
        .unwrap();
    assert_eq!(hosts.len(), SystemReason::MicrosoftServer.hosts().len());
    for host in &hosts {
        assert_eq!(host.reason, SystemReason::MicrosoftServer);
        assert_eq!(host.workspace, None);
        assert_ne!(host.note, "");
    }
    assert!(
        SystemReason::MicrosoftServer
            .describe()
            .contains("Microsoft's VS Code server")
    );
    assert_eq!(
        store.rules_every_user(&puddle_store::testing::all_users()),
        Vec::new()
    );
    // Not a set: it can't be listed or switched as one.
    assert!(matches!(
        store.rule_set(OWNER, RuleSetId::System),
        Err(StoreError::UnknownRuleSet(_))
    ));
}

#[test]
fn set_ids_parse_only_in_their_written_form() {
    assert_eq!(parse_rule_set("user:7"), Some(RuleSetId::User(7)));
    assert_eq!(parse_rule_set("user:+7"), None);
    assert_eq!(parse_rule_set("user:"), None);
    assert_eq!(
        parse_rule_set("builtin:github"),
        Some(RuleSetId::BuiltIn("github"))
    );
    assert_eq!(parse_rule_set("builtin:gone"), None);
    assert_eq!(parse_rule_set("system"), Some(RuleSetId::System));
    assert_eq!(parse_rule_set("other"), None);
}

#[test]
fn r38_the_list_has_your_sets_after_the_built_in_ones_and_only_users_change_them() {
    let (_, store) = fixture();
    let b = user_set(&store, "b set");
    let a = user_set(&store, "A set");
    let ids: Vec<RuleSetId> = store
        .rule_sets(OWNER)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids.len(), BUILT_IN_SETS.len() + 2);
    assert_eq!(
        &ids[BUILT_IN_SETS.len()..],
        [RuleSetId::User(a), RuleSetId::User(b)]
    );
    assert!(matches!(
        store.update_rule_set(OWNER, a, "x", "", None, Actor::System),
        Err(StoreError::SystemActor)
    ));
    assert!(matches!(
        store.delete_rule_set(OWNER, a, Actor::System),
        Err(StoreError::SystemActor)
    ));
}

#[test]
fn r37_stored_switches_and_reasons_this_version_cannot_read_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let clock = Arc::new(ManualClock::new(T0));
    own_workspaces(&Store::open(&path, clock.clone(), Limits::default()).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "INSERT INTO rule_set_switches (rule_set, workspace_id, enabled, changed_at, user_id) VALUES
            ('builtin:gone', NULL, 1, 1, 1),
            ('builtin:github', 'Not A Name', 1, 1, 1),
            ('builtin:github', 'a', 1, 1, 1);
         INSERT INTO system_reasons (workspace_id, reason) VALUES
            (NULL, 'a_later_reason'),
            ('Not A Name', 'code_server'),
            ('a', 'code_server');",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path, clock, Limits::default()).unwrap();
    // The readable rows count; the others are skipped.
    assert!(decide(&store, "a", "github.com").is_allow());
    assert!(!decide(&store, "b", "github.com").is_allow());
    let hosts = store
        .system_managed_every_user(&puddle_store::testing::all_users())
        .unwrap();
    assert!(hosts.iter().all(|h| h.workspace == Some(sb("a"))));
    assert!(decide(&store, "a", "open-vsx.org").is_allow());
}

#[test]
fn r37_a_workspace_override_beats_the_global_switch_which_beats_the_default() {
    let (_, store) = fixture();
    let id = user_set(&store, "Layers");
    let set = RuleSetId::User(id);
    let (a, b) = (sb("a"), sb("b"));
    let is_on = |w: &WorkspaceId| store.rule_set(OWNER, set).unwrap().is_on(w);
    // A set the user made is on by default, for everyone.
    assert!(is_on(&a) && is_on(&b));
    // The global switch decides where no override says otherwise.
    store
        .switch_rule_set(OWNER, set, None, Some(false), Actor::Ui)
        .unwrap();
    assert!(!is_on(&a) && !is_on(&b));
    // An override turns one workspace on, and only that one.
    store
        .switch_rule_set(OWNER, set, Some(&a), Some(true), Actor::Ui)
        .unwrap();
    assert!(is_on(&a) && !is_on(&b));
    // An override turns one off while the rest follow a global on.
    store
        .switch_rule_set(OWNER, set, None, Some(true), Actor::Ui)
        .unwrap();
    store
        .switch_rule_set(OWNER, set, Some(&a), Some(false), Actor::Ui)
        .unwrap();
    assert!(!is_on(&a) && is_on(&b));
    // Back to following the global switch, then the default.
    store
        .switch_rule_set(OWNER, set, Some(&a), None, Actor::Ui)
        .unwrap();
    assert!(is_on(&a));
    store
        .switch_rule_set(OWNER, set, None, None, Actor::Ui)
        .unwrap();
    assert!(is_on(&a) && is_on(&b));
}

#[test]
fn r38_a_name_of_64_characters_and_a_description_of_500_are_the_longest_allowed() {
    let (_, store) = fixture();
    let longest = store
        .create_rule_set(OWNER, &"x".repeat(64), &"d".repeat(500), None, Actor::Ui)
        .unwrap();
    assert_eq!(longest.name.chars().count(), 64);
    assert_eq!(longest.description.chars().count(), 500);
    // Counted in characters, not bytes.
    store
        .create_rule_set(OWNER, &"é".repeat(64), &"é".repeat(500), None, Actor::Ui)
        .unwrap();
}

#[test]
fn a_set_without_a_chosen_colour_gets_the_default_of_its_number() {
    let (_, store) = fixture();
    let one = store
        .create_rule_set(OWNER, "One", "", None, Actor::Ui)
        .unwrap();
    let two = store
        .create_rule_set(OWNER, "Two", "", None, Actor::Ui)
        .unwrap();
    assert_eq!(one.fill, Fill::default_for_user_set(1));
    assert_eq!(two.fill, Fill::default_for_user_set(2));
    assert_ne!(one.fill, two.fill);
    // The default is not stored: the audit record has no colour.
    assert!(
        audit_of(&store, "rule_set_created")[0]["rule_set"]
            .get("fill")
            .is_none()
    );
}

#[test]
fn a_chosen_colour_is_kept_changed_and_audited() {
    let (_, store) = fixture();
    let teal = Fill::solid(Rgb::new(0x0a, 0x6a, 0x85));
    let rose = Fill::solid(Rgb::new(0xc0, 0x4f, 0x7a));
    let made = store
        .create_rule_set(OWNER, "Client", "", Some(teal), Actor::Ui)
        .unwrap();
    assert_eq!(made.fill, teal);
    let RuleSetId::User(id) = made.id else {
        panic!("{:?}", made.id)
    };
    // A rename that names no colour keeps the one it has.
    let renamed = store
        .update_rule_set(OWNER, id, "Client X", "", None, Actor::Ui)
        .unwrap();
    assert_eq!(renamed.fill, teal);
    let recoloured = store
        .update_rule_set(OWNER, id, "Client X", "", Some(rose), Actor::Ui)
        .unwrap();
    assert_eq!(recoloured.fill, rose);
    // It survives a read of the list and shows in the audit before and after.
    let listed = store.rule_sets(OWNER).unwrap();
    assert_eq!(listed.iter().find(|s| s.id == made.id).unwrap().fill, rose);
    let updated = audit_of(&store, "rule_set_updated");
    assert_eq!(
        updated[1]["before"]["fill"],
        serde_json::json!({"kind": "solid", "color": "#0a6a85"})
    );
    assert_eq!(
        updated[1]["rule_set"]["fill"],
        serde_json::json!({"kind": "solid", "color": "#c04f7a"})
    );
}

#[test]
fn every_built_in_set_shows_its_catalogue_colour() {
    let (_, store) = fixture();
    let listed = store.rule_sets(OWNER).unwrap();
    for built in BUILT_IN_SETS {
        let shown = listed
            .iter()
            .find(|s| s.id == RuleSetId::BuiltIn(built.slug))
            .unwrap();
        assert_eq!(shown.fill, built.fill, "{}", built.slug);
    }
}

fn hide_mise() -> SystemPlan {
    SystemPlan {
        hidden_sets: [puddle_store::MISE_TOOLS_SLUG].into(),
        ..SystemPlan::default()
    }
}

fn mise_set() -> RuleSetId {
    RuleSetId::BuiltIn(puddle_store::MISE_TOOLS_SLUG)
}

#[test]
fn m8_the_mise_tools_set_ships_off_with_the_measured_hosts() {
    let (_, store) = fixture();
    let sets = store.rule_sets(OWNER).unwrap();
    let set = sets.iter().find(|s| s.id == mise_set()).unwrap();
    assert!(!set.on_by_default());
    let mut hosts: Vec<&str> = set.entries.iter().map(|e| e.pattern.as_str()).collect();
    hosts.sort_unstable();
    assert_eq!(
        hosts,
        [
            "dl.google.com",
            "github.com",
            "mise-java.jdx.dev",
            "mise-versions.jdx.dev",
            "mise.jdx.dev",
            "nodejs.org",
            "release-assets.githubusercontent.com",
            "tuf-repo-cdn.sigstore.dev",
        ]
    );
    assert!(matches!(
        decide(&store, "a", "nodejs.org"),
        Decision::Pending(_)
    ));
    store
        .switch_rule_set(OWNER, mise_set(), None, Some(true), Actor::Ui)
        .unwrap();
    for host in ["mise.jdx.dev", "nodejs.org", "dl.google.com"] {
        assert!(decide(&store, "a", host).is_allow(), "{host}");
    }
}

#[test]
fn m8_a_hidden_set_decides_nothing_and_is_not_listed() {
    let (_, store) = fixture();
    store
        .switch_rule_set(OWNER, mise_set(), None, Some(true), Actor::Ui)
        .unwrap();
    assert!(decide(&store, "a", "nodejs.org").is_allow());
    store.set_system_managed(&hide_mise()).unwrap();
    assert!(matches!(
        decide(&store, "a", "nodejs.org"),
        Decision::Pending(_)
    ));
    let sets = store.rule_sets(OWNER).unwrap();
    assert_eq!(sets.len(), BUILT_IN_SETS.len() - 1);
    assert!(sets.iter().all(|s| s.id != mise_set()));
    assert!(matches!(
        store.rule_set(OWNER, mise_set()),
        Err(StoreError::UnknownRuleSet(_))
    ));
    // Another built-in set that shares a host still decides: only the hidden set's own entries go.
    store
        .switch_rule_set(
            OWNER,
            RuleSetId::BuiltIn("github"),
            None,
            Some(true),
            Actor::Ui,
        )
        .unwrap();
    assert!(decide(&store, "a", "github.com").is_allow());
}

#[test]
fn m8_a_hidden_set_cannot_be_switched_and_its_stored_switch_survives() {
    let (_, store) = fixture();
    store
        .switch_rule_set(OWNER, mise_set(), None, Some(true), Actor::Ui)
        .unwrap();
    store
        .switch_rule_set(OWNER, mise_set(), Some(&sb("a")), Some(false), Actor::Ui)
        .unwrap();
    store.set_system_managed(&hide_mise()).unwrap();
    assert!(matches!(
        store.switch_rule_set(OWNER, mise_set(), None, Some(false), Actor::Ui),
        Err(StoreError::RuleSetHidden(id)) if id == "builtin:mise-tools"
    ));
    assert!(matches!(
        store.switch_builtin_entry(OWNER, mise_set(), "nodejs.org", false, Actor::Ui),
        Err(StoreError::RuleSetHidden(id)) if id == "builtin:mise-tools"
    ));
    // Showing it again returns the set as the user left it: on everywhere, off in "a".
    store.set_system_managed(&SystemPlan::default()).unwrap();
    let sets = store.rule_sets(OWNER).unwrap();
    let set = sets.iter().find(|s| s.id == mise_set()).unwrap();
    assert_eq!(set.global, Some(true));
    assert_eq!(set.overrides, [(sb("a"), false)]);
    assert!(decide(&store, "b", "nodejs.org").is_allow());
    assert!(matches!(
        decide(&store, "a", "nodejs.org"),
        Decision::Pending(_)
    ));
}

#[test]
fn m8_showing_a_switched_on_set_again_closes_the_requests_it_now_allows() {
    let (_, store) = fixture();
    store
        .switch_rule_set(OWNER, mise_set(), None, Some(true), Actor::Ui)
        .unwrap();
    store.set_system_managed(&hide_mise()).unwrap();
    let waiting = pending(decide(&store, "a", "nodejs.org"));
    let closed = store.set_system_managed(&SystemPlan::default()).unwrap();
    assert_eq!(closed, [waiting]);
}

#[test]
fn m8_the_hidden_state_is_stored_and_a_repeated_plan_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let clock = Arc::new(ManualClock::new(T0));
    let store = Store::open(&path, clock.clone(), Limits::default()).unwrap();
    own(&store, &sb("a"));
    store
        .switch_rule_set(OWNER, mise_set(), None, Some(true), Actor::Ui)
        .unwrap();
    store.set_system_managed(&hide_mise()).unwrap();
    drop(store);
    let reopened = Store::open(&path, clock, Limits::default()).unwrap();
    assert!(matches!(
        decide(&reopened, "a", "nodejs.org"),
        Decision::Pending(_)
    ));
    let closed = reopened.set_system_managed(&hide_mise()).unwrap();
    assert_eq!(closed.len(), 0);
    // A slug this version doesn't ship is not stored.
    let plan = SystemPlan {
        hidden_sets: ["nope"].into(),
        ..SystemPlan::default()
    };
    reopened.set_system_managed(&plan).unwrap();
    assert_eq!(
        reopened.rule_sets(OWNER).unwrap().len(),
        BUILT_IN_SETS.len()
    );
}

#[test]
fn r65_a_built_in_entry_switches_off_alone_and_back_on() {
    let (_, store) = fixture();
    let github = RuleSetId::BuiltIn("github");
    store
        .switch_rule_set(OWNER, github, None, Some(true), Actor::Ui)
        .unwrap();
    // One entry off: the others of the set still allow.
    let closed = store
        .switch_builtin_entry(OWNER, github, "codeload.github.com", false, Actor::Ui)
        .unwrap();
    assert_eq!(closed.len(), 0);
    let waiting = pending(decide(&store, "a", "codeload.github.com"));
    assert!(matches!(
        decide(&store, "a", "api.github.com"),
        Decision::SetAllow { .. }
    ));
    let info = store.rule_set(OWNER, github).unwrap();
    let off: Vec<_> = info
        .entries
        .iter()
        .filter(|e| !e.enabled)
        .map(|e| e.pattern.as_str())
        .collect();
    assert_eq!(off, ["codeload.github.com"]);
    // On again: the request that waits for it closes.
    let closed = store
        .switch_builtin_entry(OWNER, github, "codeload.github.com", true, Actor::Ui)
        .unwrap();
    assert_eq!(closed, vec![waiting]);
    assert!(
        store
            .rule_set(OWNER, github)
            .unwrap()
            .entries
            .iter()
            .all(|e| e.enabled)
    );
    let switched = audit_of(&store, "rule_switched");
    assert_eq!(switched.len(), 2);
    assert_eq!(switched[0]["kind"], "builtin_entry");
    assert_eq!(switched[0]["set_id"], "builtin:github");
    assert_eq!(switched[0]["pattern"], "codeload.github.com");
    assert_eq!(switched[0]["enabled"], false);
    assert_eq!(switched[1]["enabled"], true);
}

#[test]
fn r65_a_built_in_entry_switch_is_the_users_own_and_only_for_real_entries() {
    let (_, store) = fixture();
    let github = RuleSetId::BuiltIn("github");
    store
        .switch_rule_set(OWNER, github, None, Some(true), Actor::Ui)
        .unwrap();
    assert!(matches!(
        store.switch_builtin_entry(OWNER, github, "codeload.github.com", false, Actor::System),
        Err(StoreError::SystemActor)
    ));
    assert!(matches!(
        store.switch_builtin_entry(OWNER, RuleSetId::System, "github.com", false, Actor::Ui),
        Err(StoreError::NotSwitchable)
    ));
    assert!(matches!(
        store.switch_builtin_entry(OWNER, RuleSetId::User(1), "github.com", false, Actor::Ui),
        Err(StoreError::UnknownRuleSet(_))
    ));
    assert!(matches!(
        store.switch_builtin_entry(OWNER, github, "not-in-the-set.example", false, Actor::Ui),
        Err(StoreError::UnknownRuleSet(_))
    ));
    assert!(matches!(
        store.switch_builtin_entry(OWNER, github, "not a host", false, Actor::Ui),
        Err(StoreError::Pattern(_))
    ));
    assert!(matches!(
        store.switch_builtin_entry(
            OWNER,
            RuleSetId::BuiltIn("no-such-set"),
            "github.com",
            false,
            Actor::Ui
        ),
        Err(StoreError::UnknownRuleSet(_))
    ));
    // Switching off an entry that is already off, or on one that is on, writes nothing.
    store
        .switch_builtin_entry(OWNER, github, "codeload.github.com", true, Actor::Ui)
        .unwrap();
    store
        .switch_builtin_entry(OWNER, github, "codeload.github.com", false, Actor::Ui)
        .unwrap();
    store
        .switch_builtin_entry(OWNER, github, "codeload.github.com", false, Actor::Ui)
        .unwrap();
    assert_eq!(audit_of(&store, "rule_switched").len(), 1);
}

fn prec(value: u8) -> Precedence {
    Precedence::new(value).unwrap()
}

fn entry_precedences(store: &Store, set: i64) -> Vec<(String, u8)> {
    store
        .rule_set(OWNER, RuleSetId::User(set))
        .unwrap()
        .entries
        .into_iter()
        .map(|e| (e.pattern, e.precedence))
        .collect()
}

#[test]
fn r59_a_new_rule_in_a_group_takes_the_groups_default_and_old_ones_keep_theirs() {
    let (_, store) = fixture();
    let set = user_set(&store, "Mine");
    let user = store.as_user(OWNER);
    assert_eq!(
        store
            .rule_set(OWNER, RuleSetId::User(set))
            .unwrap()
            .default_precedence,
        None
    );
    user.add_rule(&new_rule(Scope::Set(set), "one.example.com", Effect::Allow))
        .unwrap();
    let changed = store
        .set_rule_set_default_precedence(OWNER, set, Some(prec(8)), Actor::Ui)
        .unwrap();
    assert_eq!(changed.default_precedence, Some(prec(8)));
    user.add_rule(&new_rule(Scope::Set(set), "two.example.com", Effect::Allow))
        .unwrap();
    // A named precedence beats the group's default.
    user.add_rule_with_precedence(
        &new_rule(Scope::Set(set), "three.example.com", Effect::Allow),
        prec(2),
    )
    .unwrap();
    // A rule outside the group is not touched by the group's default.
    let outside = user
        .add_rule(&new_rule(Scope::Global, "four.example.com", Effect::Allow))
        .unwrap();
    assert_eq!(outside.precedence, Precedence::DEFAULT);
    assert_eq!(
        entry_precedences(&store, set),
        vec![
            ("one.example.com".to_owned(), 5),
            ("two.example.com".to_owned(), 8),
            ("three.example.com".to_owned(), 2),
        ]
    );
    // Clearing it goes back to 5 for the next rule, and changes nothing already there.
    store
        .set_rule_set_default_precedence(OWNER, set, None, Actor::Ui)
        .unwrap();
    user.add_rule(&new_rule(
        Scope::Set(set),
        "five.example.com",
        Effect::Allow,
    ))
    .unwrap();
    assert_eq!(
        entry_precedences(&store, set)[1],
        ("two.example.com".to_owned(), 8)
    );
    assert_eq!(
        entry_precedences(&store, set)[3],
        ("five.example.com".to_owned(), 5)
    );
}

#[test]
fn r59_approving_a_request_into_a_group_takes_the_groups_default() {
    let (_, store) = fixture();
    let set = user_set(&store, "Mine");
    store
        .set_rule_set_default_precedence(OWNER, set, Some(prec(7)), Actor::Ui)
        .unwrap();
    let row = pending(decide(&store, "a", "approve.example.com"));
    let mut into_set = Resolution::allow();
    into_set.scope = ScopeChoice::Set(set);
    let decided = store
        .as_user(puddle_store::UserId::OWNER)
        .resolve_pending(row, &into_set, Actor::Ui)
        .unwrap();
    assert_eq!(decided.rule.precedence, prec(7));
}

#[test]
fn r59_the_default_is_audited_once_and_a_repeat_writes_nothing() {
    let (_, store) = fixture();
    let set = user_set(&store, "Mine");
    for _ in 0..2 {
        store
            .set_rule_set_default_precedence(OWNER, set, Some(prec(3)), Actor::Api)
            .unwrap();
    }
    let updates = audit_of(&store, "rule_set_updated");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["rule_set"]["default_precedence"], 3);
    assert!(updates[0]["before"].get("default_precedence").is_none());
}

#[test]
fn r59_a_default_precedence_is_the_users_own_and_never_the_systems() {
    let (_, store) = fixture();
    let set = user_set(&store, "Mine");
    assert!(matches!(
        store.set_rule_set_default_precedence(OWNER, set, Some(prec(4)), Actor::System),
        Err(StoreError::SystemActor)
    ));
    assert!(matches!(
        store.set_rule_set_default_precedence(OWNER, 999, Some(prec(4)), Actor::Ui),
        Err(StoreError::UnknownRuleSet(_))
    ));
    // Built-in entries sit at 0, below every rule of the user.
    let github = store.rule_set(OWNER, RuleSetId::BuiltIn("github")).unwrap();
    assert!(github.entries.iter().all(|e| e.precedence == 0));
    assert_eq!(github.default_precedence, None);
}

#[test]
fn switching_a_set_to_the_state_it_is_in_writes_nothing_and_closes_nothing() {
    let (_, store) = fixture();
    let set = RuleSetId::BuiltIn("github");
    let switch = |enabled: Option<bool>| {
        store
            .switch_rule_set(OWNER, set, None, enabled, Actor::Ui)
            .unwrap()
    };
    switch(None);
    assert_eq!(
        audit_of(&store, "rule_set_switched").len(),
        0,
        "no override yet"
    );
    switch(Some(true));
    assert_eq!(audit_of(&store, "rule_set_switched").len(), 1);
    let waiting = pending(decide(&store, "a", "example.org"));
    switch(Some(true));
    assert_eq!(audit_of(&store, "rule_set_switched").len(), 1, "already on");
    assert_eq!(
        store.pending(waiting).unwrap().state,
        PendingState::Requested
    );
    switch(Some(false));
    switch(None);
    assert_eq!(audit_of(&store, "rule_set_switched").len(), 3);
}

#[test]
fn another_user_cannot_switch_or_delete_a_set_or_use_a_workspace_that_is_not_theirs() {
    let (_, store) = fixture();
    let ann = store
        .admin_add_user(OWNER, &puddle_store::UserName::new("ann").unwrap())
        .unwrap()
        .id;
    let mine = user_set(&store, "Mine");
    assert!(matches!(
        store.switch_rule_set(ann, RuleSetId::User(mine), None, Some(true), Actor::Ui),
        Err(StoreError::UnknownRuleSet(_))
    ));
    assert!(matches!(
        store.switch_rule_set(
            ann,
            RuleSetId::BuiltIn("github"),
            Some(&sb("a")),
            Some(true),
            Actor::Ui
        ),
        Err(StoreError::UnknownWorkspace(_))
    ));
    assert!(matches!(
        store.delete_rule_set(ann, mine, Actor::Ui),
        Err(StoreError::UnknownRuleSet(_))
    ));
    assert!(store.rule_set(OWNER, RuleSetId::User(mine)).is_ok());
    assert_eq!(audit_of(&store, "rule_set_switched").len(), 0);
    assert_eq!(audit_of(&store, "rule_set_deleted").len(), 0);
}

#[test]
fn deleting_a_set_returns_it_as_it_was_with_its_entries_and_switches() {
    let (_, store) = fixture();
    let set = user_set(&store, "Mine");
    store
        .switch_rule_set(OWNER, RuleSetId::User(set), None, Some(true), Actor::Ui)
        .unwrap();
    for host in ["a.example", "b.example"] {
        store
            .as_user(OWNER)
            .add_rule(&new_rule(Scope::Set(set), host, Effect::Allow))
            .unwrap();
    }
    let info = store.delete_rule_set(OWNER, set, Actor::Ui).unwrap();
    assert_eq!(info.name, "Mine");
    assert_eq!(info.entries.len(), 2);
    assert_eq!(info.global, Some(true));
    assert!(store.rule_set(OWNER, RuleSetId::User(set)).is_err());
}
