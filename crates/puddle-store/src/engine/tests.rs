// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::rule::Actor;
use proptest::prelude::*;
use puddle_types::PatternKind;

pub(crate) fn rule(id: i64, scope: Option<&str>, pattern: &str, effect: Effect) -> Rule {
    Rule {
        id: RuleId(id),
        user: UserId::OWNER,
        scope: scope.map_or(Scope::Global, |s| {
            Scope::Workspace(WorkspaceId::new(s).unwrap())
        }),
        pattern: Pattern::parse(pattern).unwrap(),
        effect,
        expires_at: None,
        created_at: 0,
        created_by: Actor::Cli,
        source_pending_id: None,
    }
}

/// The workspaces these tests ask for, each recorded as the owner's.
fn owners_own(names: &[&str]) -> HashMap<WorkspaceId, UserId> {
    names.iter().map(|name| (sb(name), UserId::OWNER)).collect()
}

/// An index where every workspace the tests name is the owner's.
fn new_index(rules: Vec<Rule>, entries: Vec<SetEntry>, switches: Switches) -> RuleIndex {
    RuleIndex::new(
        rules,
        entries,
        switches,
        owners_own(&["a", "b", "c", "mine", "other", "ssh"]),
    )
}

fn sb(s: &str) -> WorkspaceId {
    WorkspaceId::new(s).unwrap()
}

fn host(s: &str) -> Host {
    Host::parse_normalised(s).unwrap()
}

fn index(rules: Vec<Rule>) -> RuleIndex {
    new_index(rules, Vec::new(), Switches::default())
}

fn winner(set: &RuleIndex, workspace: &str, h: &str) -> Option<i64> {
    set.decide(&sb(workspace), &host(h), 1_000, SuffixAllows::Count)
        .and_then(Hit::rule_id)
        .map(|id| id.0)
}

#[test]
fn r01_empty_rule_set_decides_nothing() {
    let set = RuleIndex::default();
    assert_eq!(set.rules().len(), 0);
    assert_eq!(winner(&set, "a", "example.com"), None);
}

#[test]
fn a_workspace_with_no_user_is_decided_by_no_rule() {
    let github = RuleSetId::BuiltIn("github");
    let mut switches = Switches::default();
    switches.insert(UserId::OWNER, github, None, true);
    let set = RuleIndex::new(
        vec![
            rule(1, None, "example.com", Effect::Allow),
            rule(2, Some("orphan"), "example.com", Effect::Allow),
        ],
        vec![
            system("example.com", None),
            built_in("github", "github.com"),
        ],
        switches,
        owners_own(&["a"]),
    );
    assert_eq!(winner(&set, "a", "example.com"), Some(1));
    assert!(set.is_on(github, &sb("a")));
    assert_eq!(set.owner_of(&sb("orphan")), None);
    assert_eq!(decision(&set, "orphan", "example.com"), None);
    assert_eq!(decision(&set, "orphan", "github.com"), None);
    assert!(!set.is_on(github, &sb("orphan")));
}

#[test]
fn r05_other_workspaces_rules_never_apply() {
    let set = index(vec![
        rule(1, Some("other"), "example.com", Effect::Allow),
        rule(2, None, "global.example", Effect::Allow),
    ]);
    assert_eq!(winner(&set, "mine", "example.com"), None);
    assert_eq!(winner(&set, "other", "example.com"), Some(1));
    assert_eq!(winner(&set, "mine", "global.example"), Some(2));
}

#[test]
fn r06_exact_beats_suffix_and_longer_suffix_beats_shorter() {
    let set = index(vec![
        rule(1, Some("a"), ".example.com", Effect::Deny),
        rule(2, None, ".api.example.com", Effect::Allow),
        rule(3, None, "x.api.example.com", Effect::Deny),
    ]);
    assert_eq!(winner(&set, "a", "y.api.example.com"), Some(2));
    assert_eq!(winner(&set, "a", "x.api.example.com"), Some(3));
    assert_eq!(winner(&set, "a", "other.example.com"), Some(1));
}

#[test]
fn r06_workspace_beats_global_at_equal_specificity() {
    let set = index(vec![
        rule(1, None, ".example.com", Effect::Deny),
        rule(2, Some("a"), ".example.com", Effect::Allow),
    ]);
    assert_eq!(winner(&set, "a", "x.example.com"), Some(2));
    assert_eq!(winner(&set, "b", "x.example.com"), Some(1));
}

#[test]
fn r06_broader_workspace_allow_never_overrides_narrower_global_deny() {
    let set = index(vec![
        rule(1, None, ".api.example.com", Effect::Deny),
        rule(2, Some("a"), ".example.com", Effect::Allow),
    ]);
    assert_eq!(winner(&set, "a", "x.api.example.com"), Some(1));
}

#[test]
fn r06_deny_beats_allow_at_equal_scope() {
    let set = index(vec![
        rule(1, Some("a"), "example.com", Effect::Allow),
        rule(2, Some("a"), "example.com", Effect::Deny),
        rule(3, Some("a"), "example.com", Effect::Allow),
    ]);
    assert_eq!(winner(&set, "a", "example.com"), Some(2));
}

#[test]
fn scope_rank_leaves_room_below_global() {
    assert_eq!(Scope::Global.rank(), 1);
    assert!(Scope::Workspace(sb("a")).rank() > Scope::Global.rank());
    assert!(Scope::Set(1).rank() < Scope::Global.rank());
}

fn in_set(id: i64, set: i64, pattern: &str, effect: Effect) -> Rule {
    let mut r = rule(id, None, pattern, effect);
    r.scope = Scope::Set(set);
    r
}

/// Two set denies of one pattern: the lower set, then the lower rule id, decides, whichever the
/// index met first; it matters when an own rule is less specific and a set deny overrides it.
#[test]
fn of_two_set_denies_that_override_an_own_rule_the_lower_set_wins_whatever_the_order() {
    let own = rule(9, None, ".example.com", Effect::Allow);
    for denies in [
        vec![
            in_set(1, 2, "x.example.com", Effect::Deny),
            in_set(2, 1, "x.example.com", Effect::Deny),
        ],
        vec![
            in_set(2, 1, "x.example.com", Effect::Deny),
            in_set(1, 2, "x.example.com", Effect::Deny),
        ],
    ] {
        let mut rules = vec![own.clone()];
        rules.extend(denies);
        let got = decision(&index(rules), "a", "x.example.com");
        assert_eq!(
            got,
            Some(Decision::SetDeny {
                set: RuleSetId::User(1),
                rule_id: Some(RuleId(2)),
                pattern: PatternKind::Exact,
            })
        );
    }
}

fn system(pattern: &str, workspace: Option<&str>) -> SetEntry {
    SetEntry {
        set: RuleSetId::System,
        pattern: Pattern::parse(pattern).unwrap(),
        workspace: workspace.map(sb),
        effect: Effect::Allow,
    }
}

fn built_in(slug: &'static str, pattern: &str) -> SetEntry {
    SetEntry {
        set: RuleSetId::BuiltIn(slug),
        pattern: Pattern::parse(pattern).unwrap(),
        workspace: None,
        effect: Effect::Allow,
    }
}

/// An entry of a built-in deny set.
fn built_in_deny(slug: &'static str, pattern: &str) -> SetEntry {
    SetEntry {
        effect: Effect::Deny,
        ..built_in(slug, pattern)
    }
}

fn decision(index: &RuleIndex, workspace: &str, h: &str) -> Option<Decision> {
    index
        .decide(&sb(workspace), &host(h), 1_000, SuffixAllows::Count)
        .map(Hit::decision)
}

fn own_allow(id: i64, pattern: PatternKind) -> Decision {
    Decision::Allow {
        rule_id: RuleId(id),
        pattern,
    }
}

#[test]
fn r39_example_one_a_set_allow_never_opens_what_your_rule_closes() {
    // You deny *.visualstudio.com everywhere; System managed allows the update host.
    let index = new_index(
        vec![rule(1, None, ".visualstudio.com", Effect::Deny)],
        vec![system("update.code.visualstudio.com", None)],
        Switches::default(),
    );
    assert_eq!(
        decision(&index, "a", "update.code.visualstudio.com"),
        Some(Decision::Deny {
            rule_id: RuleId(1),
            pattern: PatternKind::Suffix
        })
    );
    // Without your rule, the set's allow fills the gap.
    let index = new_index(
        Vec::new(),
        vec![system("update.code.visualstudio.com", None)],
        Switches::default(),
    );
    assert_eq!(
        decision(&index, "a", "update.code.visualstudio.com"),
        Some(Decision::SetAllow {
            set: RuleSetId::System,
            rule_id: None,
            pattern: PatternKind::Exact
        })
    );
}

#[test]
fn r39_example_two_a_set_block_competes_at_its_level_of_detail() {
    // You allow *.example.com; your own set blocks ads.example.com.
    let blocked = new_index(
        vec![
            rule(1, None, ".example.com", Effect::Allow),
            in_set(2, 7, "ads.example.com", Effect::Deny),
        ],
        Vec::new(),
        Switches::default(),
    );
    assert_eq!(
        decision(&blocked, "a", "ads.example.com"),
        Some(Decision::SetDeny {
            set: RuleSetId::User(7),
            rule_id: Some(RuleId(2)),
            pattern: PatternKind::Exact
        })
    );
    assert_eq!(
        decision(&blocked, "a", "www.example.com"),
        Some(own_allow(1, PatternKind::Suffix))
    );
    // Your exact allow of the same host still wins.
    let mut rules = blocked.rules().to_vec();
    rules.push(rule(3, Some("a"), "ads.example.com", Effect::Allow));
    let yours = new_index(rules, Vec::new(), Switches::default());
    assert_eq!(
        decision(&yours, "a", "ads.example.com"),
        Some(own_allow(3, PatternKind::Exact))
    );
    // At equal detail your rule ranks above the set's block.
    let equal = new_index(
        vec![
            rule(1, None, ".example.com", Effect::Allow),
            in_set(2, 7, ".example.com", Effect::Deny),
        ],
        Vec::new(),
        Switches::default(),
    );
    assert_eq!(
        decision(&equal, "a", "x.example.com"),
        Some(own_allow(1, PatternKind::Suffix))
    );
}

#[test]
fn the_plain_precedence_lets_a_more_specific_set_block_beat_your_rule() {
    // The property test reaches this arm of the reference only by chance, so pin it: the
    // reference and the index must both pick the set's exact block over your suffix allow.
    let index = new_index(
        vec![
            rule(1, None, ".example.com", Effect::Allow),
            in_set(2, 7, "ads.example.com", Effect::Deny),
        ],
        Vec::new(),
        Switches::default(),
    );
    let (name, h) = (sb("a"), host("ads.example.com"));
    let expected = Some(Decision::SetDeny {
        set: RuleSetId::User(7),
        rule_id: Some(RuleId(2)),
        pattern: PatternKind::Exact,
    });
    assert_eq!(
        reference_p3(&index, &name, &h, 5, SuffixAllows::Count),
        expected
    );
    assert_eq!(
        index
            .decide(&name, &h, 5, SuffixAllows::Count)
            .map(Hit::decision),
        expected
    );
}

#[test]
fn r39_between_sets_most_specific_then_deny_wins() {
    let index = new_index(
        vec![
            in_set(1, 1, ".example.com", Effect::Deny),
            in_set(2, 2, "x.example.com", Effect::Allow),
            in_set(3, 2, ".example.com", Effect::Allow),
        ],
        vec![built_in("github", "y.example.com")],
        {
            let mut s = Switches::default();
            s.insert(UserId::OWNER, RuleSetId::BuiltIn("github"), None, true);
            s
        },
    );
    let set_of = |h| decision(&index, "a", h).and_then(|d| d.rule_set());
    assert_eq!(set_of("x.example.com"), Some(RuleSetId::User(2)));
    assert_eq!(set_of("y.example.com"), Some(RuleSetId::BuiltIn("github")));
    assert!(matches!(
        decision(&index, "a", "z.example.com"),
        Some(Decision::SetDeny { .. })
    ));
}

#[test]
fn a_users_global_rules_sets_and_switches_decide_only_their_own_workspaces() {
    let second = UserId::from_stored(2);
    let mut theirs = rule(2, None, "example.com", Effect::Deny);
    theirs.user = second;
    let mut their_set = in_set(3, 9, "set.example", Effect::Allow);
    their_set.user = second;
    let mut switches = Switches::default();
    switches.insert(second, RuleSetId::BuiltIn("github"), None, true);
    switches.insert(second, RuleSetId::BuiltIn("github"), Some(sb("b")), false);
    let index = RuleIndex::new(
        vec![
            rule(1, None, "example.com", Effect::Allow),
            theirs,
            their_set,
        ],
        vec![built_in("github", "github.com")],
        switches,
        HashMap::from([
            (sb("a"), UserId::OWNER),
            (sb("b"), second),
            (sb("c"), second),
        ]),
    );
    // The owner's workspace `a`: the owner's global rule, no set of the second user, the
    // built-in set as it ships (off).
    assert_eq!(winner(&index, "a", "example.com"), Some(1));
    assert_eq!(decision(&index, "a", "set.example"), None);
    assert_eq!(decision(&index, "a", "github.com"), None);
    // The second user's workspaces: their own global rule and set, their own switches.
    assert_eq!(winner(&index, "b", "example.com"), Some(2));
    assert_eq!(winner(&index, "c", "set.example"), Some(3));
    assert_eq!(decision(&index, "b", "github.com"), None);
    assert!(decision(&index, "c", "github.com").is_some());
    assert_eq!(index.owner_of(&sb("a")), Some(UserId::OWNER));
    assert_eq!(
        index.overrides(second, RuleSetId::BuiltIn("github")),
        vec![(sb("b"), false)]
    );
    assert_eq!(
        index.overrides(UserId::OWNER, RuleSetId::BuiltIn("github")),
        Vec::new()
    );
    assert_eq!(
        index
            .switches()
            .global(second, RuleSetId::BuiltIn("github")),
        Some(true)
    );
    assert_eq!(
        index
            .switches()
            .global(UserId::OWNER, RuleSetId::BuiltIn("github")),
        None
    );
}

#[test]
fn r37_switches_decide_where_a_set_applies() {
    let rules = vec![in_set(1, 5, "example.com", Effect::Allow)];
    let entries = vec![built_in("github", "github.com")];
    let mut switches = Switches::default();
    // A set you made is on by default; a built-in set ships off.
    let index = new_index(rules.clone(), entries.clone(), switches.clone());
    assert!(decision(&index, "a", "example.com").is_some());
    assert_eq!(decision(&index, "a", "github.com"), None);
    // Global on, one workspace off; the workspace's value wins there.
    switches.insert(UserId::OWNER, RuleSetId::BuiltIn("github"), None, true);
    switches.insert(
        UserId::OWNER,
        RuleSetId::BuiltIn("github"),
        Some(sb("b")),
        false,
    );
    switches.insert(UserId::OWNER, RuleSetId::User(5), None, false);
    switches.insert(UserId::OWNER, RuleSetId::User(5), Some(sb("b")), true);
    let index = new_index(rules, entries, switches);
    assert!(decision(&index, "a", "github.com").is_some());
    assert_eq!(decision(&index, "b", "github.com"), None);
    assert_eq!(decision(&index, "a", "example.com"), None);
    assert!(decision(&index, "b", "example.com").is_some());
    assert_eq!(
        index.switches().global(UserId::OWNER, RuleSetId::User(5)),
        Some(false)
    );
    assert_eq!(
        index.overrides(UserId::OWNER, RuleSetId::BuiltIn("github")),
        vec![(sb("b"), false)]
    );
    assert_eq!(index.entries().len(), 1);
    assert!(default_on(RuleSetId::User(9)) && default_on(RuleSetId::System));
    assert!(!default_on(RuleSetId::BuiltIn("github")) && !default_on(RuleSetId::BuiltIn("gone")));
}

#[test]
fn r41_system_entries_for_one_workspace_stay_there() {
    let index = new_index(
        Vec::new(),
        vec![system("marketplace.visualstudio.com", Some("ssh"))],
        Switches::default(),
    );
    assert!(decision(&index, "ssh", "marketplace.visualstudio.com").is_some());
    assert_eq!(
        decision(&index, "other", "marketplace.visualstudio.com"),
        None
    );
}

#[test]
fn r42_ignore_mode_drops_every_set_allow_and_keeps_set_denies() {
    let index = new_index(
        vec![
            in_set(1, 1, "db.corp.example", Effect::Allow),
            in_set(2, 1, "bad.corp.example", Effect::Deny),
        ],
        vec![system("open-vsx.org", None)],
        Switches::default(),
    );
    let ignore = |h: &str| {
        index
            .decide(&sb("a"), &host(h), 0, SuffixAllows::Ignore)
            .map(Hit::decision)
    };
    assert_eq!(ignore("db.corp.example"), None);
    assert_eq!(ignore("open-vsx.org"), None);
    assert!(matches!(
        ignore("bad.corp.example"),
        Some(Decision::SetDeny { .. })
    ));
}

fn on(set: RuleSetId) -> Switches {
    let mut switches = Switches::default();
    switches.insert(UserId::OWNER, set, None, true);
    switches
}

#[test]
fn r39_a_built_in_deny_set_blocks_only_while_it_is_on() {
    let set = RuleSetId::BuiltIn("trackers");
    let entries = || vec![built_in_deny("trackers", "*.tracker.example")];
    let off = new_index(Vec::new(), entries(), Switches::default());
    assert_eq!(decision(&off, "a", "x.tracker.example"), None);
    let live = new_index(Vec::new(), entries(), on(set));
    assert_eq!(
        decision(&live, "a", "x.tracker.example"),
        Some(Decision::SetDeny {
            set,
            rule_id: None,
            pattern: PatternKind::Suffix,
        })
    );
    // It applies in suffix-ignore mode too: a deny is never dropped there (R-42).
    assert!(matches!(
        live.decide(
            &sb("a"),
            &host("x.tracker.example"),
            0,
            SuffixAllows::Ignore
        )
        .map(Hit::decision),
        Some(Decision::SetDeny { rule_id: None, .. })
    ));
    // The user's own allow of the exact name beats it; their allow of the same suffix does too.
    for pattern in ["x.tracker.example", ".tracker.example"] {
        let own = new_index(
            vec![rule(1, None, pattern, Effect::Allow)],
            entries(),
            on(set),
        );
        assert_eq!(winner(&own, "a", "x.tracker.example"), Some(1));
    }
}

#[test]
fn r39_a_mixed_set_decides_by_specificity_then_deny() {
    let mixed = || {
        new_index(
            vec![
                in_set(1, 7, ".example.com", Effect::Allow),
                in_set(2, 7, "ads.example.com", Effect::Deny),
                in_set(3, 7, ".cdn.example.com", Effect::Deny),
                in_set(4, 7, ".cdn.example.com", Effect::Allow),
                in_set(5, 7, "ok.cdn.example.com", Effect::Allow),
            ],
            Vec::new(),
            Switches::default(),
        )
    };
    let id = |h: &str| winner(&mixed(), "a", h);
    assert_eq!(id("www.example.com"), Some(1));
    assert_eq!(id("ads.example.com"), Some(2));
    assert_eq!(id("img.cdn.example.com"), Some(3));
    assert_eq!(id("ok.cdn.example.com"), Some(5));
}

#[test]
fn r07_expired_set_entries_never_match() {
    let mut expired = in_set(1, 1, "example.com", Effect::Deny);
    expired.expires_at = Some(1_000);
    let index = new_index(vec![expired], Vec::new(), Switches::default());
    assert_eq!(decision(&index, "a", "example.com"), None);
}

#[test]
fn identical_rules_tie_on_lower_id() {
    let set = index(vec![
        rule(5, None, "example.com", Effect::Allow),
        rule(4, None, "example.com", Effect::Allow),
    ]);
    assert_eq!(winner(&set, "a", "example.com"), Some(4));
}

#[test]
fn r07_expired_rule_never_matches() {
    let mut expired = rule(1, None, "example.com", Effect::Allow);
    expired.expires_at = Some(1_000);
    let mut live = rule(2, None, ".example.com", Effect::Deny);
    live.expires_at = Some(1_001);
    let set = index(vec![expired, live]);
    // At now == expires_at the rule is already expired.
    assert_eq!(winner(&set, "a", "example.com"), None);
    assert_eq!(winner(&set, "a", "x.example.com"), Some(2));
    assert_eq!(
        set.decide(&sb("a"), &host("x.example.com"), 1_001, SuffixAllows::Count)
            .map(Hit::rule_id),
        None
    );
}

#[test]
fn r14_ignore_mode_drops_suffix_allows_but_keeps_suffix_denies() {
    let set = index(vec![
        rule(1, Some("a"), ".example.com", Effect::Allow),
        rule(2, None, ".example.com", Effect::Deny),
        rule(3, None, ".corp.example", Effect::Allow),
        rule(4, None, "db.corp.example", Effect::Allow),
    ]);
    let ignore = |h: &str| {
        set.decide(&sb("a"), &host(h), 0, SuffixAllows::Ignore)
            .map(|hit| (hit.rule_id().unwrap().0, hit.pattern().kind()))
    };
    assert_eq!(ignore("x.example.com"), Some((2, PatternKind::Suffix)));
    assert_eq!(ignore("web.corp.example"), None);
    assert_eq!(ignore("db.corp.example"), Some((4, PatternKind::Exact)));
}

#[test]
fn ip_literals_match_only_exact_rules() {
    let set = index(vec![rule(1, None, "10.0.0.1", Effect::Allow)]);
    assert_eq!(winner(&set, "a", "10.0.0.1"), Some(1));
    assert_eq!(winner(&set, "a", "10.0.0.2"), None);
}

/// The precedence of R-6, written as plainly as possible, for the property test below.
fn reference(rules: &[Rule], workspace: &WorkspaceId, h: &Host, now: u64) -> Option<RuleId> {
    let mut best: Option<&Rule> = None;
    for r in rules {
        let applies = r.pattern.matches(h)
            && !r.is_expired(now)
            && r.scope.workspace().is_none_or(|s| s == workspace);
        if !applies {
            continue;
        }
        let key = |r: &Rule| {
            (
                r.pattern.specificity(),
                r.scope.rank(),
                r.effect == Effect::Deny,
                Reverse(r.id),
            )
        };
        if best.is_none_or(|b| key(r) > key(b)) {
            best = Some(r);
        }
    }
    best.map(|r| r.id)
}

fn arb_rule() -> impl Strategy<Value = Rule> {
    (
        0..50i64,
        prop_oneof![Just(None), Just(Some("a")), Just(Some("b"))],
        prop_oneof![
            Just("x.y.example.com"),
            Just("y.example.com"),
            Just(".y.example.com"),
            Just(".example.com"),
            Just("example.com"),
            Just("10.0.0.1"),
        ],
        any::<bool>(),
        proptest::option::of(0..20u64),
    )
        .prop_map(|(id, scope, pattern, deny, expires)| {
            let effect = if deny { Effect::Deny } else { Effect::Allow };
            let mut r = rule(id, scope, pattern, effect);
            r.expires_at = expires;
            r
        })
}

/// R-39 written as plainly as possible, for the property test below: every candidate
/// checked one by one.
fn reference_p3(
    index: &RuleIndex,
    workspace: &WorkspaceId,
    h: &Host,
    now: u64,
    suffix_allows: SuffixAllows,
) -> Option<Decision> {
    // A workspace that is nobody's has no rules.
    index.owner_of(workspace)?;
    let ignore = suffix_allows == SuffixAllows::Ignore;
    let own: Vec<&Rule> = index
        .rules()
        .iter()
        .filter(|r| r.pattern.matches(h) && !r.is_expired(now))
        .filter(|r| {
            (matches!(r.scope, Scope::Global) && Some(r.user) == index.owner_of(workspace))
                || r.scope.workspace() == Some(workspace)
        })
        .filter(|r| {
            !(ignore && r.effect == Effect::Allow && r.pattern.kind() == PatternKind::Suffix)
        })
        .collect();
    let mut sets: Vec<Hit<'_>> = Vec::new();
    for r in index.rules() {
        if let Scope::Set(set) = r.scope
            && Some(r.user) == index.owner_of(workspace)
            && r.pattern.matches(h)
            && !r.is_expired(now)
            && index.is_on(RuleSetId::User(set), workspace)
            && !(ignore && r.effect == Effect::Allow)
        {
            sets.push(Hit::Rule(r));
        }
    }
    for e in index.entries() {
        let on = match e.set {
            RuleSetId::System => e.workspace.as_ref().is_none_or(|s| s == workspace),
            set => index.is_on(set, workspace),
        };
        if on && !ignore && e.pattern.matches(h) {
            sets.push(Hit::Entry(e));
        }
    }
    let best_set = sets.iter().copied().max_by_key(|hit| set_key(*hit));
    let best_deny = sets
        .iter()
        .copied()
        .filter(|hit| hit.effect() == Effect::Deny)
        .max_by_key(|hit| set_key(*hit));
    match own.into_iter().max_by_key(|r| own_key(r)) {
        Some(r) => match best_deny {
            Some(d) if d.pattern().specificity() > r.pattern.specificity() => Some(d.decision()),
            _ => Some(Hit::Rule(r).decision()),
        },
        None => best_set.map(Hit::decision),
    }
}

#[test]
fn a_more_specific_set_deny_beats_a_less_specific_rule_of_the_user() {
    let own = rule(1, None, "example.com", Effect::Allow);
    let mut deny = rule(2, None, "x.y.example.com", Effect::Deny);
    deny.scope = Scope::Set(1);
    let index = new_index(vec![own, deny], Vec::new(), Switches::default());
    let expected = Decision::SetDeny {
        set: RuleSetId::User(1),
        rule_id: Some(RuleId(2)),
        pattern: PatternKind::Exact,
    };
    let args = (&sb("a"), &host("x.y.example.com"), 0, SuffixAllows::Count);
    let (workspace, h, now, mode) = args;
    assert_eq!(
        index.decide(workspace, h, now, mode).map(Hit::decision),
        Some(expected)
    );
    assert_eq!(
        reference_p3(&index, workspace, h, now, mode),
        Some(expected)
    );
}

fn arb_scope() -> impl Strategy<Value = Scope> {
    prop_oneof![
        Just(Scope::Global),
        Just(Scope::Workspace(sb("a"))),
        Just(Scope::Workspace(sb("b"))),
        Just(Scope::Set(1)),
        Just(Scope::Set(2)),
    ]
}

fn arb_pattern() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("x.y.example.com"),
        Just("y.example.com"),
        Just(".y.example.com"),
        Just(".example.com"),
        Just("example.com"),
        Just("10.0.0.1"),
    ]
}

fn arb_index() -> impl Strategy<Value = RuleIndex> {
    let rules = proptest::collection::vec(
        (
            (0..50i64, 1..=2i64),
            arb_scope(),
            arb_pattern(),
            any::<bool>(),
            proptest::option::of(0..20u64),
        ),
        0..10,
    );
    let entries = proptest::collection::vec(
        (
            prop_oneof![Just(RuleSetId::System), Just(RuleSetId::BuiltIn("github"))],
            arb_pattern(),
            prop_oneof![Just(None), Just(Some("a")), Just(Some("b"))],
        ),
        0..4,
    );
    let switches = proptest::collection::vec(
        (
            prop_oneof![
                Just(RuleSetId::User(1)),
                Just(RuleSetId::User(2)),
                Just(RuleSetId::BuiltIn("github"))
            ],
            prop_oneof![Just(None), Just(Some("a")), Just(Some("b"))],
            any::<bool>(),
            1..=2i64,
        ),
        0..5,
    );
    // Workspace `a` is the owner's; `b` the second user's, the owner's or nobody's.
    let b_owner = prop_oneof![Just(None), Just(Some(1i64)), Just(Some(2i64))];
    (rules, entries, switches, b_owner).prop_map(|(rules, entries, switches, b_owner)| {
        let mut owners = owners_own(&["a"]);
        if let Some(user) = b_owner {
            owners.insert(sb("b"), UserId::from_stored(user));
        }
        let user_of = |scope: &Scope, user: i64| match scope.workspace() {
            Some(w) => owners.get(w).copied().unwrap_or(UserId::from_stored(user)),
            None => UserId::from_stored(user),
        };
        let rules = rules
            .into_iter()
            .map(|((id, user), scope, pattern, deny, expires)| {
                let effect = if deny { Effect::Deny } else { Effect::Allow };
                let mut r = rule(id, None, pattern, effect);
                r.user = user_of(&scope, user);
                r.scope = scope;
                r.expires_at = expires;
                r
            })
            .collect();
        let entries = entries
            .into_iter()
            .map(|(set, pattern, workspace)| SetEntry {
                set,
                pattern: Pattern::parse(pattern).unwrap(),
                workspace: if set == RuleSetId::System {
                    workspace.map(sb)
                } else {
                    None
                },
                effect: Effect::Allow,
            })
            .collect();
        let mut on = Switches::default();
        for (set, workspace, enabled, user) in switches {
            on.insert(UserId::from_stored(user), set, workspace.map(sb), enabled);
        }
        RuleIndex::new(rules, entries, on, owners)
    })
}

proptest! {
    #[test]
    fn index_agrees_with_the_plain_precedence(
        rules in proptest::collection::vec(arb_rule(), 0..12),
        workspace in prop_oneof![Just("a"), Just("b")],
        h in prop_oneof![
            Just("x.y.example.com"), Just("y.example.com"), Just("example.com"),
            Just("z.example.com"), Just("10.0.0.1"),
        ],
        now in 0..25u64,
    ) {
        let set = index(rules.clone());
        let got = set.decide(&sb(workspace), &host(h), now, SuffixAllows::Count).and_then(Hit::rule_id);
        prop_assert_eq!(got, reference(&rules, &sb(workspace), &host(h), now));
    }

    #[test]
    fn r39_index_agrees_with_the_plain_precedence_with_sets(
        index in arb_index(),
        workspace in prop_oneof![Just("a"), Just("b")],
        h in prop_oneof![
            Just("x.y.example.com"), Just("y.example.com"), Just("example.com"),
            Just("z.example.com"), Just("10.0.0.1"),
        ],
        now in 0..25u64,
        ignore in any::<bool>(),
    ) {
        let mode = if ignore { SuffixAllows::Ignore } else { SuffixAllows::Count };
        let got = index.decide(&sb(workspace), &host(h), now, mode).map(Hit::decision);
        prop_assert_eq!(got, reference_p3(&index, &sb(workspace), &host(h), now, mode));
    }

    #[test]
    fn r39_a_set_never_opens_what_an_own_rule_closes(
        index in arb_index(),
        workspace in prop_oneof![Just("a"), Just("b")],
        h in prop_oneof![Just("x.y.example.com"), Just("y.example.com"), Just("example.com")],
    ) {
        let own_only = RuleIndex::new(
            index.rules().iter().filter(|r| r.scope.set().is_none()).cloned().collect(),
            Vec::new(),
            Switches::default(),
            index.owners.clone(),
        );
        let mine = own_only.decide(&sb(workspace), &host(h), 5, SuffixAllows::Count).map(Hit::effect);
        let all = index.decide(&sb(workspace), &host(h), 5, SuffixAllows::Count).map(Hit::effect);
        if mine == Some(Effect::Deny) {
            prop_assert_eq!(all, Some(Effect::Deny));
        }
    }

    #[test]
    fn a_rule_or_switch_of_another_user_never_decides(
        index in arb_index(),
        workspace in prop_oneof![Just("a"), Just("b")],
        h in prop_oneof![
            Just("x.y.example.com"), Just("y.example.com"), Just("example.com"),
            Just("z.example.com"), Just("10.0.0.1"),
        ],
        now in 0..25u64,
    ) {
        let workspace = sb(workspace);
        // `None` for a workspace that is nobody's, whose requests no rule decides.
        let user = index.owner_of(&workspace);
        if let Some(Hit::Rule(rule)) = index.decide(&workspace, &host(h), now, SuffixAllows::Count) {
            prop_assert_eq!(Some(rule.user), user);
        }
        // Dropping every other user's rules and switches changes no answer.
        let mut switches = Switches::default();
        for ((u, set), on) in &index.switches.global {
            if Some(*u) == user {
                switches.insert(*u, *set, None, *on);
            }
        }
        for ((set, w), on) in &index.switches.workspace {
            if let Some(user) = user {
                switches.insert(user, *set, Some(w.clone()), *on);
            }
        }
        let alone = RuleIndex::new(
            index.rules().iter().filter(|r| Some(r.user) == user).cloned().collect(),
            index.entries.clone(),
            switches,
            index.owners.clone(),
        );
        prop_assert_eq!(
            index.decide(&workspace, &host(h), now, SuffixAllows::Count).map(Hit::decision),
            alone.decide(&workspace, &host(h), now, SuffixAllows::Count).map(Hit::decision)
        );
    }

    #[test]
    fn r07_expired_rules_never_decide(
        rules in proptest::collection::vec(arb_rule(), 0..12),
        now in 0..25u64,
    ) {
        let set = index(rules);
        for h in ["x.y.example.com", "example.com", "10.0.0.1"] {
            if let Some(Hit::Rule(r)) = set.decide(&sb("a"), &host(h), now, SuffixAllows::Count) {
                prop_assert!(!r.is_expired(now));
            }
        }
    }
}
