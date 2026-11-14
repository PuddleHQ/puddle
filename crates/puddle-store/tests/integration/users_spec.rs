// SPDX-License-Identifier: AGPL-3.0-or-later
//! Several users in one store: each user's rules, rule sets, switches, rules on method and path,
//! identities, Git defaults, environment and audit records belong to them alone, and the rules of
//! one user never decide a request of another user's workspace.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail the test by panicking"
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use puddle_secrets::{AccountName, HostName, SourceSpec};
use puddle_store::{
    Actor, AuditCursor, AuditFilter, Author, Coverage, CredentialBinding, EnvDraft, EnvName,
    EnvScope, IdentityDraft, Limits, ManualClock, NewRequestRule, NewRule, Owner, Pattern,
    PatternChoice, Resolution, Scope, ScopeChoice, Store, StoreError, SystemPlan, SystemReason,
    UserId, UserName,
};
use puddle_types::{
    CollectingSink, Decision, EgressRequest, Event, Host, Methods, PathPattern, PendingOutcome,
    RuleSetId, SuffixAllows, WorkspaceId,
};

const T0: u64 = 1_800_000_000_000;

/// A store with the owner's workspace `mine` and Ann's workspace `anns`.
fn fixture() -> (Store, UserId, Arc<CollectingSink>) {
    let events = Arc::new(CollectingSink::default());
    let store = Store::open_in_memory(Arc::new(ManualClock::new(T0)), Limits::default())
        .unwrap()
        .with_events(events.clone());
    let ann = store.add_user(&UserName::new("Ann").unwrap()).unwrap().id;
    store.set_workspace_owner(&ws("anns"), ann).unwrap();
    (store, ann, events)
}

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id).unwrap()
}

fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap()
}

fn decide(store: &Store, workspace: &str, name: &str) -> Decision {
    store
        .decide(
            &EgressRequest::new(ws(workspace), host(name), 443),
            SuffixAllows::Count,
        )
        .unwrap()
}

fn allows(store: &Store, workspace: &str, name: &str) -> bool {
    matches!(
        decide(store, workspace, name),
        Decision::Allow { .. } | Decision::SetAllow { .. }
    )
}

fn rule(scope: Scope, pattern: &str, effect: puddle_store::Effect) -> NewRule {
    NewRule {
        scope,
        pattern: Pattern::parse(pattern).unwrap(),
        effect,
        expires_at: None,
        created_by: Actor::Api,
    }
}

fn allow(scope: Scope, pattern: &str) -> NewRule {
    rule(scope, pattern, puddle_store::Effect::Allow)
}

fn draft(label: &str) -> IdentityDraft {
    let host = HostName::new("github.com").unwrap();
    let owners: BTreeSet<Owner> = [Owner::new("someone").unwrap()].into();
    IdentityDraft {
        label: label.to_owned(),
        author: Author::new(label, "me@example.com").unwrap(),
        credentials: vec![
            CredentialBinding::new(
                &host,
                SourceSpec::Gh {
                    host: host.clone(),
                    account: AccountName::new("me").unwrap(),
                },
                Coverage::new(owners, false).unwrap(),
            )
            .unwrap(),
        ],
    }
}

#[test]
fn a_global_rule_decides_only_the_workspaces_of_the_user_who_made_it() {
    let (store, ann, _) = fixture();
    store
        .as_user(UserId::OWNER)
        .add_rule(&allow(Scope::Global, "owners.example"))
        .unwrap();
    let anns = store
        .as_user(ann)
        .add_rule(&allow(Scope::Global, "anns.example"))
        .unwrap();
    assert_eq!(anns.user, ann);
    assert!(allows(&store, "mine", "owners.example"));
    assert!(allows(&store, "anns", "anns.example"));
    assert!(matches!(
        decide(&store, "anns", "owners.example"),
        Decision::Pending(_)
    ));
    assert!(matches!(
        decide(&store, "mine", "anns.example"),
        Decision::Pending(_)
    ));
    assert_eq!(store.rules(ann), vec![anns]);
    assert!(
        store
            .rules(UserId::OWNER)
            .iter()
            .all(|r| r.user == UserId::OWNER)
    );
}

#[test]
fn a_rule_for_another_users_workspace_or_set_is_refused_as_unknown() {
    let (store, ann, _) = fixture();
    let err = store
        .as_user(ann)
        .add_rule(&allow(Scope::Workspace(ws("mine")), "x.example"))
        .unwrap_err();
    assert!(matches!(err, StoreError::UnknownWorkspace(w) if w == ws("mine")));
    let owners_set = store
        .create_rule_set(UserId::OWNER, "Mine", "", None, Actor::Api)
        .unwrap();
    let RuleSetId::User(set) = owners_set.id else {
        unreachable!()
    };
    let err = store
        .as_user(ann)
        .add_rule(&allow(Scope::Set(set), "x.example"))
        .unwrap_err();
    assert!(matches!(err, StoreError::UnknownRuleSet(_)));
    // The owner does not write into ann's workspace either; ann does, and the rule is hers.
    let err = store
        .as_user(UserId::OWNER)
        .add_rule(&allow(Scope::Workspace(ws("anns")), "y.example"))
        .unwrap_err();
    assert!(matches!(err, StoreError::UnknownWorkspace(w) if w == ws("anns")));
    let made = store
        .as_user(ann)
        .add_rule(&allow(Scope::Workspace(ws("anns")), "y.example"))
        .unwrap();
    assert_eq!(made.user, ann);
}

#[test]
fn approving_for_every_workspace_makes_a_rule_of_the_workspaces_user() {
    let (store, ann, _) = fixture();
    let Decision::Pending(PendingOutcome::New(id)) = decide(&store, "anns", "api.example") else {
        unreachable!()
    };
    let decided = store
        .resolve_pending(
            id,
            &Resolution {
                effect: puddle_store::Effect::Allow,
                pattern: PatternChoice::Exact,
                scope: ScopeChoice::Global,
                expires_in: None,
            },
            Actor::Api,
        )
        .unwrap();
    assert_eq!(
        (decided.rule.scope, decided.rule.user),
        (Scope::Global, ann)
    );
    assert!(allows(&store, "anns", "api.example"));
    assert!(matches!(
        decide(&store, "mine", "api.example"),
        Decision::Pending(_)
    ));
}

#[test]
fn recording_a_workspaces_user_changes_whose_rules_decide_at_once() {
    let (store, ann, _) = fixture();
    store
        .as_user(UserId::OWNER)
        .add_rule(&allow(Scope::Global, "owners.example"))
        .unwrap();
    assert!(allows(&store, "late", "owners.example"));
    store.set_workspace_owner(&ws("late"), ann).unwrap();
    assert!(!allows(&store, "late", "owners.example"));
}

#[test]
fn rule_sets_and_switches_are_one_users() {
    let (store, ann, _) = fixture();
    let owners = store
        .create_rule_set(UserId::OWNER, "Work", "", None, Actor::Api)
        .unwrap();
    // The same name is free for another user.
    let anns = store
        .create_rule_set(ann, "Work", "", None, Actor::Api)
        .unwrap();
    let RuleSetId::User(anns_id) = anns.id else {
        unreachable!()
    };
    store
        .as_user(ann)
        .add_rule(&allow(Scope::Set(anns_id), "set.example"))
        .unwrap();
    let ids = |user| -> Vec<RuleSetId> {
        store
            .rule_sets(user)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .filter(|id| matches!(id, RuleSetId::User(_)))
            .collect()
    };
    assert_eq!(ids(UserId::OWNER), vec![owners.id]);
    assert_eq!(ids(ann), vec![anns.id]);
    assert!(matches!(
        store.rule_set(UserId::OWNER, anns.id),
        Err(StoreError::UnknownRuleSet(_))
    ));
    assert!(matches!(
        store.delete_rule_set(UserId::OWNER, anns_id, Actor::Api),
        Err(StoreError::UnknownRuleSet(_))
    ));
    assert!(matches!(
        store.update_rule_set(UserId::OWNER, anns_id, "x", "", None, Actor::Api),
        Err(StoreError::UnknownRuleSet(_))
    ));
    // Ann's set (on by default) allows only for Ann.
    assert!(allows(&store, "anns", "set.example"));
    assert!(!allows(&store, "mine", "set.example"));
    // The owner switching a built-in set on for every workspace changes only the owner's.
    let github = RuleSetId::BuiltIn("github");
    let waiting = |workspace: &str| match decide(&store, workspace, "github.com") {
        Decision::Pending(PendingOutcome::New(id)) => id,
        other => unreachable!("{other:?}"),
    };
    let (owners_request, anns_request) = (waiting("mine"), waiting("anns"));
    let closed = store
        .switch_rule_set(UserId::OWNER, github, None, Some(true), Actor::Api)
        .unwrap();
    // Only the owner's request is closed, and only it is reported.
    assert_eq!(closed, vec![owners_request]);
    assert_eq!(
        store.pending(anns_request).unwrap().state,
        puddle_store::PendingState::Requested
    );
    assert!(allows(&store, "mine", "github.com"));
    assert!(!allows(&store, "anns", "github.com"));
    let github_for = |user| store.rule_set(user, github).unwrap().global;
    assert_eq!(
        (github_for(UserId::OWNER), github_for(ann)),
        (Some(true), None)
    );
    assert!(matches!(
        store.switch_rule_set(ann, github, Some(&ws("mine")), Some(true), Actor::Api),
        Err(StoreError::UnknownWorkspace(_))
    ));
    store
        .switch_rule_set(ann, github, Some(&ws("anns")), Some(true), Actor::Api)
        .unwrap();
    assert!(allows(&store, "anns", "github.com"));
    assert_eq!(
        store.rule_set(ann, github).unwrap().overrides,
        vec![(ws("anns"), true)]
    );
    assert_eq!(
        store.rule_set(UserId::OWNER, github).unwrap().overrides,
        Vec::new()
    );
}

#[test]
fn rules_on_method_and_path_apply_to_their_users_workspaces_only() {
    let (store, ann, _) = fixture();
    let new = |workspace: Option<&str>| NewRequestRule {
        workspace: workspace.map(ws),
        host: "api.example".to_owned(),
        methods: Methods::parse(["DELETE"]).unwrap(),
        path: PathPattern::parse("/x").unwrap(),
        created_by: Actor::Api,
    };
    let owners = store
        .as_user(UserId::OWNER)
        .add_request_rule(&new(None))
        .unwrap();
    let anns = store.as_user(ann).add_request_rule(&new(None)).unwrap();
    let ids = |workspace: &str| -> Vec<i64> {
        store
            .request_rules_for(&ws(workspace))
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect()
    };
    assert_eq!(ids("mine"), vec![owners.id]);
    assert_eq!(ids("anns"), vec![anns.id]);
    assert_eq!(store.request_rules(ann, None).unwrap(), vec![anns]);
    assert!(matches!(
        store.as_user(ann).add_request_rule(&new(Some("mine"))),
        Err(StoreError::UnknownWorkspace(_))
    ));
}

#[test]
fn identities_and_git_defaults_are_one_users() {
    let (store, ann, _) = fixture();
    let owners = store.create_identity(UserId::OWNER, draft("Work")).unwrap();
    // Ann's first identity is her default, and the label is free for her.
    let anns = store.create_identity(ann, draft("Work")).unwrap();
    assert!(owners.is_default && anns.is_default);
    assert_eq!(store.identities(ann).unwrap(), vec![anns.clone()]);
    assert!(matches!(
        store.identity(ann, owners.id),
        Err(StoreError::UnknownIdentity(_))
    ));
    assert!(matches!(
        store.delete_identity(ann, owners.id),
        Err(StoreError::UnknownIdentity(_))
    ));
    assert!(matches!(
        store.set_workspace_identities(&ws("anns"), &[owners.id]),
        Err(StoreError::UnknownIdentity(_))
    ));
    assert_eq!(
        store
            .set_workspace_identities(&ws("anns"), &[anns.id])
            .unwrap()
            .identities,
        vec![anns]
    );
    // The push list is on by default; Ann turns hers off.
    store.set_git_defaults(ann, Some(false), None).unwrap();
    assert!(!store.workspace_git(&ws("anns")).unwrap().only_push_listed);
    assert!(store.workspace_git(&ws("mine")).unwrap().only_push_listed);
    assert!(store.git_defaults(UserId::OWNER).unwrap().only_push_listed);
}

#[test]
fn the_global_environment_is_one_users() {
    let (store, ann, _) = fixture();
    let name = EnvName::new("COLOUR").unwrap();
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &name,
            EnvDraft::plain("blue").unwrap(),
        )
        .unwrap();
    store
        .set_env(
            ann,
            &EnvScope::Global,
            &name,
            EnvDraft::plain("red").unwrap(),
        )
        .unwrap();
    let value = |workspace: &str| {
        let rows = store.workspace_env(&ws(workspace)).unwrap();
        assert_eq!(rows.len(), 1);
        format!("{:?}", rows[0].entry.value)
    };
    assert!(value("mine").contains("blue"));
    assert!(value("anns").contains("red"));
    let started = store
        .env_for_start(&ws("anns"), &mut |_| Ok(String::new()))
        .unwrap();
    assert!(format!("{started:?}").contains("red"));
    assert!(matches!(
        store.env_entries(ann, &EnvScope::Workspace(ws("mine"))),
        Err(StoreError::UnknownWorkspace(_))
    ));
    assert!(matches!(
        store.delete_env(UserId::OWNER, &EnvScope::Workspace(ws("anns")), &name),
        Err(StoreError::UnknownWorkspace(_))
    ));
    store.delete_env(ann, &EnvScope::Global, &name).unwrap();
    assert_eq!(
        store
            .env_entries(UserId::OWNER, &EnvScope::Global)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn audit_records_and_their_events_reach_the_user_they_belong_to() {
    let (store, ann, events) = fixture();
    let _ = events.take();
    let made = store
        .as_user(ann)
        .add_rule(&allow(Scope::Global, "anns.example"))
        .unwrap();
    let appended: Vec<Event> = events
        .take()
        .into_iter()
        .filter(|e| matches!(e, Event::AuditAppended { .. }))
        .collect();
    let page = |user| {
        store
            .audit_query_every_user(
                &AuditFilter {
                    user: Some(user),
                    ..AuditFilter::default()
                },
                AuditCursor::Before(None),
                10,
            )
            .unwrap()
    };
    let anns = page(ann);
    assert_eq!(anns.len(), 1);
    assert!(anns[0].1.contains("anns.example"));
    assert_eq!(
        appended,
        vec![Event::AuditAppended {
            id: anns[0].0,
            user: ann.get(),
        }]
    );
    assert!(
        page(UserId::OWNER)
            .iter()
            .all(|(_, line)| !line.contains("anns.example"))
    );
    // Deleting it is recorded for Ann too.
    store.delete_rule(made.id, Actor::Api).unwrap();
    assert_eq!(page(ann).len(), 2);
    assert!(events.take().iter().all(|e| match e {
        Event::AuditAppended { user, .. } => *user == ann.get(),
        _ => true,
    }));
}

#[test]
fn one_change_with_records_of_two_users_tells_each_of_them() {
    let (store, ann, events) = fixture();
    assert!(matches!(
        decide(&store, "anns", "open-vsx.org"),
        Decision::Pending(_)
    ));
    assert!(matches!(
        decide(&store, "mine", "open-vsx.org"),
        Decision::Pending(_)
    ));
    let _ = events.take();
    // System managed is the install's: it closes both users' requests in one change.
    let closed = store
        .set_system_managed(&SystemPlan {
            everywhere: [SystemReason::CodeServer].into(),
            ..SystemPlan::default()
        })
        .unwrap();
    assert_eq!(closed.len(), 2);
    let mut users: Vec<i64> = events
        .take()
        .into_iter()
        .filter_map(|e| match e {
            Event::AuditAppended { user, .. } => Some(user),
            _ => None,
        })
        .collect();
    users.sort_unstable();
    assert_eq!(users, vec![UserId::OWNER.get(), ann.get()]);
}

/// Every list read takes the user it lists for and holds that user's things alone; a workspace
/// with no recorded user is in nobody's list (only the reads for the install's own jobs, named
/// `*_every_user`, see it).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one list read after another, one story"
)]
fn every_list_read_keeps_to_one_user_and_a_workspace_with_no_user_to_nobody() {
    let (store, ann, _) = fixture();
    store
        .set_workspace_owner(&ws("mine"), UserId::OWNER)
        .unwrap();
    for workspace in ["mine", "anns", "stray"] {
        assert!(matches!(
            decide(&store, workspace, &format!("{workspace}.example")),
            Decision::Pending(_)
        ));
    }
    let workspaces = |rows: Vec<puddle_store::PendingRow>| -> BTreeSet<String> {
        rows.into_iter().map(|r| r.workspace.to_string()).collect()
    };
    let set =
        |names: &[&str]| -> BTreeSet<String> { names.iter().map(|n| (*n).to_owned()).collect() };
    assert_eq!(
        workspaces(store.open_pending(UserId::OWNER, None).unwrap()),
        set(&["mine"])
    );
    assert_eq!(
        workspaces(store.open_pending(ann, None).unwrap()),
        set(&["anns"])
    );
    assert_eq!(
        workspaces(store.open_pending(ann, Some(&ws("mine"))).unwrap()),
        set(&[])
    );
    assert_eq!(
        workspaces(store.open_pending(ann, Some(&ws("anns"))).unwrap()),
        set(&["anns"])
    );
    assert_eq!(
        workspaces(store.open_pending_every_user(None).unwrap()),
        set(&["mine", "anns", "stray"])
    );
    let inbox = |groups: Vec<puddle_store::InboxGroup>| -> BTreeSet<String> {
        groups
            .into_iter()
            .flat_map(|g| g.rows)
            .map(|r| r.workspace.to_string())
            .collect()
    };
    assert_eq!(inbox(store.inbox(ann).unwrap()), set(&["anns"]));
    assert_eq!(inbox(store.inbox(UserId::OWNER).unwrap()), set(&["mine"]));
    assert_eq!(
        inbox(store.inbox_every_user().unwrap()),
        set(&["mine", "anns", "stray"])
    );

    // Rules on method and path: Ann's, for her workspace only.
    let new = |workspace: Option<&str>| NewRequestRule {
        workspace: workspace.map(ws),
        host: "api.example".to_owned(),
        methods: Methods::parse(["DELETE"]).unwrap(),
        path: PathPattern::parse("/x").unwrap(),
        created_by: Actor::Api,
    };
    let owners_rule = store
        .as_user(UserId::OWNER)
        .add_request_rule(&new(Some("mine")))
        .unwrap();
    let anns_global = store.as_user(ann).add_request_rule(&new(None)).unwrap();
    let anns_own = store
        .as_user(ann)
        .add_request_rule(&new(Some("anns")))
        .unwrap();
    let ids = |rules: Vec<puddle_store::RequestRule>| -> Vec<i64> {
        rules.iter().map(|r| r.id).collect()
    };
    assert_eq!(
        ids(store.request_rules(ann, None).unwrap()),
        [anns_global.id, anns_own.id]
    );
    assert_eq!(
        ids(store.request_rules(ann, Some(&ws("anns"))).unwrap()),
        [anns_global.id, anns_own.id]
    );
    assert_eq!(
        ids(store.request_rules(ann, Some(&ws("mine"))).unwrap()),
        Vec::<i64>::new()
    );
    assert_eq!(
        ids(store.request_rules(ann, Some(&ws("stray"))).unwrap()),
        Vec::<i64>::new()
    );
    assert_eq!(
        ids(store.request_rules(UserId::OWNER, None).unwrap()),
        [owners_rule.id]
    );
    assert_eq!(
        ids(store.request_rules_every_user().unwrap()),
        [owners_rule.id, anns_global.id, anns_own.id]
    );
    assert_eq!(store.request_rule_owner(anns_own.id).unwrap(), Some(ann));
    assert_eq!(
        store.request_rule_owner(owners_rule.id).unwrap(),
        Some(UserId::OWNER)
    );
    assert_eq!(store.request_rule_owner(999_999).unwrap(), None);

    // Host rules: Ann's alone, and whose a rule is by its id.
    let rule = |workspace: &str, pattern: &str| NewRule {
        scope: Scope::Workspace(ws(workspace)),
        pattern: Pattern::parse(pattern).unwrap(),
        effect: puddle_store::Effect::Allow,
        expires_at: None,
        created_by: Actor::Api,
    };
    let owners_host = store
        .as_user(UserId::OWNER)
        .add_rule(&rule("mine", "o.example"))
        .unwrap();
    let anns_host = store
        .as_user(ann)
        .add_rule(&rule("anns", "a.example"))
        .unwrap();
    let rule_ids = |rules: Vec<puddle_store::Rule>| -> Vec<puddle_types::RuleId> {
        rules.iter().map(|r| r.id).collect()
    };
    assert_eq!(rule_ids(store.rules(ann)), [anns_host.id]);
    assert_eq!(rule_ids(store.rules(UserId::OWNER)), [owners_host.id]);
    assert_eq!(
        rule_ids(store.rules_every_user()),
        [owners_host.id, anns_host.id]
    );
    assert_eq!(store.rule_owner(anns_host.id), Some(ann));
    assert_eq!(store.rule_owner(owners_host.id), Some(UserId::OWNER));
    assert_eq!(store.rule_owner(puddle_types::RuleId(999_999)), None);

    // System managed hosts: the ones for every workspace, and the user's own workspaces'.
    let plan = SystemPlan {
        everywhere: [SystemReason::MicrosoftServer].into(),
        workspaces: [
            (ws("mine"), [SystemReason::CodeServer].into()),
            (ws("anns"), [SystemReason::CodeServer].into()),
            (ws("stray"), [SystemReason::CodeServer].into()),
        ]
        .into(),
        ..SystemPlan::default()
    };
    store.set_system_managed(&plan).unwrap();
    let scopes = |hosts: Vec<puddle_store::SystemHost>| -> BTreeSet<String> {
        hosts
            .into_iter()
            .map(|h| {
                h.workspace
                    .map_or_else(|| "*".to_owned(), |w| w.to_string())
            })
            .collect()
    };
    assert_eq!(
        scopes(store.system_managed(ann).unwrap()),
        set(&["*", "anns"])
    );
    assert_eq!(
        scopes(store.system_managed(UserId::OWNER).unwrap()),
        set(&["*", "mine"])
    );
    assert_eq!(
        scopes(store.system_managed_every_user().unwrap()),
        set(&["*", "mine", "anns", "stray"])
    );

    // Audit: the user the read is for, whatever the filter says.
    let asking_for_the_owners = AuditFilter {
        user: Some(UserId::OWNER),
        ..AuditFilter::default()
    };
    let lines = store
        .audit_query(ann, &asking_for_the_owners, AuditCursor::After(0), 1000)
        .unwrap();
    assert!(
        lines
            .iter()
            .all(|(_, line)| !line.contains("o.example") && !line.contains("\"mine\"")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|(_, line)| line.contains("a.example")),
        "{lines:?}"
    );
}

/// Every write a second user makes is stored as theirs: no row of the seven per-user tables is
/// the owner's afterwards, so a write that forgets its user cannot pass for the owner's.
#[test]
fn every_write_as_a_second_user_leaves_no_row_of_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let store = Store::open(&path, Arc::new(ManualClock::new(T0)), Limits::default()).unwrap();
    let ann = store.add_user(&UserName::new("Ann").unwrap()).unwrap().id;
    store.set_workspace_owner(&ws("anns"), ann).unwrap();

    let anns = store.as_user(ann);
    anns.add_rule(&allow(Scope::Global, "global.example"))
        .unwrap();
    anns.add_rule(&allow(Scope::Workspace(ws("anns")), "own.example"))
        .unwrap();
    let set = store
        .create_rule_set(ann, "Work", "", None, Actor::Api)
        .unwrap();
    let RuleSetId::User(set_id) = set.id else {
        unreachable!()
    };
    anns.add_rule(&allow(Scope::Set(set_id), "set.example"))
        .unwrap();
    store
        .switch_rule_set(ann, set.id, Some(&ws("anns")), Some(true), Actor::Api)
        .unwrap();
    for workspace in [None, Some("anns")] {
        anns.add_request_rule(&NewRequestRule {
            workspace: workspace.map(ws),
            host: "api.example".to_owned(),
            methods: Methods::parse(["GET"]).unwrap(),
            path: PathPattern::parse("/*").unwrap(),
            created_by: Actor::Api,
        })
        .unwrap();
    }
    store
        .set_env(
            ann,
            &EnvScope::Global,
            &EnvName::new("COLOUR").unwrap(),
            EnvDraft::plain("red").unwrap(),
        )
        .unwrap();
    store.create_identity(ann, draft("Work")).unwrap();
    let Decision::Pending(PendingOutcome::New(id)) = decide(&store, "anns", "ask.example") else {
        unreachable!()
    };
    store
        .resolve_pending(
            id,
            &Resolution {
                effect: puddle_store::Effect::Allow,
                pattern: PatternChoice::Exact,
                scope: ScopeChoice::Global,
                expires_in: None,
            },
            Actor::Api,
        )
        .unwrap();

    let conn = rusqlite::Connection::open(&path).unwrap();
    for table in [
        "rules",
        "request_rules",
        "audit",
        "rule_sets",
        "rule_set_switches",
        "env_vars",
        "identities",
    ] {
        let rows: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        let owners: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE user_id = 1"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(rows > 0, "{table}: the test wrote nothing");
        assert_eq!(owners, 0, "{table} holds a row of the owner");
    }
}
