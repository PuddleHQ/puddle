// SPDX-License-Identifier: AGPL-3.0-or-later
//! The in-memory rule index and the matching rules R-5 to R-7, R-59 and R-60. Pure: no I/O, no clock.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use puddle_types::{Decision, Host, RuleId, RuleSetId, SuffixAllows, WorkspaceId};

use crate::catalogue;
use crate::pattern::Pattern;
use crate::rule::{Effect, Precedence, Rule, Scope};
use crate::users::UserId;

/// An entry of a built-in set or of System managed: no rule row (R-36, R-41). System managed
/// entries always allow; a built-in set's entries all share the set's effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetEntry {
    /// The set it belongs to: [`RuleSetId::BuiltIn`] or [`RuleSetId::System`].
    pub set: RuleSetId,
    /// What it matches.
    pub pattern: Pattern,
    /// For System managed: the one workspace it applies to, or `None` for every workspace. Built-in
    /// entries apply where their set is switched on.
    pub workspace: Option<WorkspaceId>,
    /// Allow or deny.
    pub effect: Effect,
}

/// Which rule sets are switched on where (R-37): an override per workspace, a value for every
/// workspace of one user, and the set's own default, in that order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Switches {
    global: HashMap<(UserId, RuleSetId), bool>,
    workspace: HashMap<(RuleSetId, WorkspaceId), bool>,
    /// Built-in entries a user switched off (R-65), by the user, the set and the entry's pattern.
    off_entries: HashSet<(UserId, RuleSetId, String)>,
}

impl Switches {
    /// Records one stored switch of `user` (for a workspace's switch, the workspace's user).
    pub(crate) fn insert(
        &mut self,
        user: UserId,
        set: RuleSetId,
        workspace: Option<WorkspaceId>,
        enabled: bool,
    ) {
        match workspace {
            Some(workspace) => {
                self.workspace.insert((set, workspace), enabled);
            }
            None => {
                self.global.insert((user, set), enabled);
            }
        }
    }

    /// Records that `user` switched the entry `pattern` of the built-in `set` off.
    pub(crate) fn insert_entry_off(&mut self, user: UserId, set: RuleSetId, pattern: String) {
        self.off_entries.insert((user, set, pattern));
    }

    /// Whether `user` switched the entry `pattern` of `set` off. Only built-in entries have a
    /// switch of their own.
    fn entry_off(&self, user: UserId, set: RuleSetId, pattern: &Pattern) -> bool {
        !self.off_entries.is_empty()
            && matches!(set, RuleSetId::BuiltIn(_))
            && self.off_entries.contains(&(user, set, pattern.to_string()))
    }

    /// `user`'s switch for every workspace, if set.
    pub(crate) fn global(&self, user: UserId, set: RuleSetId) -> Option<bool> {
        self.global.get(&(user, set)).copied()
    }

    /// The workspaces that override `set`, sorted by name.
    fn overrides(&self, set: RuleSetId) -> Vec<(WorkspaceId, bool)> {
        let mut out: Vec<_> = self
            .workspace
            .iter()
            .filter(|((s, _), _)| *s == set)
            .map(|((_, workspace), on)| (workspace.clone(), *on))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Whether `set` is on for `workspace`, which belongs to `user`.
    fn is_on(&self, user: UserId, set: RuleSetId, workspace: &WorkspaceId) -> bool {
        self.workspace
            .get(&(set, workspace.clone()))
            .or_else(|| self.global.get(&(user, set)))
            .copied()
            .unwrap_or_else(|| default_on(set))
    }
}

/// A set's state where nobody switched it: a built-in set as it ships (off), a set the user made
/// on, System managed always (it has no switch).
pub(crate) fn default_on(set: RuleSetId) -> bool {
    match set {
        RuleSetId::BuiltIn(slug) => catalogue::built_in(slug).is_some_and(|s| s.default_on),
        _ => true,
    }
}

/// What decided a request: one of the rows in `rules`, or a built-in or System managed entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hit<'a> {
    /// A rule row: the user's own rule, or an entry of a set the user made.
    Rule(&'a Rule),
    /// A built-in or System managed entry.
    Entry(&'a SetEntry),
}

impl Hit<'_> {
    /// Allow or deny.
    pub(crate) fn effect(self) -> Effect {
        match self {
            Self::Rule(rule) => rule.effect,
            Self::Entry(entry) => entry.effect,
        }
    }

    /// The rule row, if any.
    pub(crate) fn rule_id(self) -> Option<RuleId> {
        match self {
            Self::Rule(rule) => Some(rule.id),
            Self::Entry(_) => None,
        }
    }

    /// The set the entry belongs to, if it is a set's entry.
    pub(crate) fn rule_set(self) -> Option<RuleSetId> {
        match self {
            Self::Rule(rule) => rule.scope.set().map(RuleSetId::User),
            Self::Entry(entry) => Some(entry.set),
        }
    }
}

impl<'a> Hit<'a> {
    fn pattern(self) -> &'a Pattern {
        match self {
            Self::Rule(rule) => &rule.pattern,
            Self::Entry(entry) => &entry.pattern,
        }
    }

    /// The engine's answer (R-9, R-39).
    pub(crate) fn decision(self) -> Decision {
        let pattern = self.pattern().kind();
        match (self, self.rule_set()) {
            (Self::Rule(rule), None) => match rule.effect {
                Effect::Allow => Decision::Allow {
                    rule_id: rule.id,
                    pattern,
                },
                Effect::Deny => Decision::Deny {
                    rule_id: rule.id,
                    pattern,
                },
            },
            (Self::Rule(rule), Some(set)) => match rule.effect {
                Effect::Allow => Decision::SetAllow {
                    set,
                    rule_id: Some(rule.id),
                    pattern,
                },
                Effect::Deny => Decision::SetDeny {
                    set,
                    rule_id: Some(rule.id),
                    pattern,
                },
            },
            (Self::Entry(entry), _) => match entry.effect {
                Effect::Allow => Decision::SetAllow {
                    set: entry.set,
                    rule_id: None,
                    pattern,
                },
                Effect::Deny => Decision::SetDeny {
                    set: entry.set,
                    rule_id: None,
                    pattern,
                },
            },
        }
    }
}

/// Entries indexed by pattern, for exact and suffix lookup. The built-in part is one of these,
/// built once and shared by every [`RuleIndex`] (S8); a snapshot adds its own for System managed.
#[derive(Debug, Default)]
pub(crate) struct EntryIndex {
    entries: Vec<SetEntry>,
    /// Exact patterns and suffix bases (`.example.com` stored under `example.com`) to positions.
    exact: HashMap<String, Vec<usize>>,
    suffix: HashMap<String, Vec<usize>>,
}

impl EntryIndex {
    pub(crate) fn new(entries: Vec<SetEntry>) -> Self {
        let mut exact: HashMap<String, Vec<usize>> = HashMap::new();
        let mut suffix: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            index_pattern(&mut exact, &mut suffix, &entry.pattern, index);
        }
        Self {
            entries,
            exact,
            suffix,
        }
    }

    /// Entries whose pattern matches `host`: its exact entry, then every proper suffix.
    fn matching<'a>(&'a self, host: &Host) -> impl Iterator<Item = &'a SetEntry> + 'a {
        positions(&self.exact, &self.suffix, host).filter_map(|index| self.entries.get(index))
    }
}

fn index_pattern(
    exact: &mut HashMap<String, Vec<usize>>,
    suffix: &mut HashMap<String, Vec<usize>>,
    pattern: &Pattern,
    position: usize,
) {
    match pattern {
        Pattern::Exact(host) => exact.entry(host.to_string()).or_default().push(position),
        Pattern::Suffix(s) => suffix
            .entry(s.base().as_str().to_owned())
            .or_default()
            .push(position),
    }
}

/// Positions whose pattern matches `host`: its exact entry, then every proper suffix.
fn positions<'a>(
    exact: &'a HashMap<String, Vec<usize>>,
    suffix: &'a HashMap<String, Vec<usize>>,
    host: &Host,
) -> impl Iterator<Item = usize> + 'a {
    let exact_hits = exact.get(&host.to_string()).into_iter().flatten().copied();
    let suffixes: Vec<usize> = match host {
        Host::Name(name) => {
            let text = name.as_str();
            text.match_indices('.')
                .filter_map(|(dot, _)| text.get(dot + 1..))
                .filter_map(|base| suffix.get(base))
                .flatten()
                .copied()
                .collect()
        }
        Host::Ip(_) => Vec::new(),
    };
    exact_hits.chain(suffixes)
}

/// An immutable snapshot of every rule, set entry, switch and workspace user, indexed for
/// matching. The store replaces it whole after each committed change (R-8), so a decision never
/// sees half a change. The built-in entries are not part of what it builds: it shares one
/// [`EntryIndex`], so a change re-indexes only rules and System managed entries (S8).
#[derive(Debug, Default)]
pub(crate) struct RuleIndex {
    rules: Vec<Rule>,
    rule_exact: HashMap<String, Vec<usize>>,
    rule_suffix: HashMap<String, Vec<usize>>,
    /// System managed entries (and, in tests, any entries given to [`RuleIndex::new`]).
    system: EntryIndex,
    /// The built-in sets' entries, shared by every snapshot.
    built_in: Arc<EntryIndex>,
    /// Slugs of built-in sets that are hidden: their shared entries decide nothing.
    hidden: HashSet<String>,
    switches: Switches,
    /// Each workspace's user. A workspace with none is nobody's: no rule decides for it.
    owners: HashMap<WorkspaceId, UserId>,
}

/// How the rules that match a request are ranked (R-60): precedence first, then pattern
/// specificity, a `workspace` rule over a `global` one (an entry of a group counts as `global`),
/// `deny` over `allow`, then a stable order that cannot change an answer.
type Key = (u8, (u8, usize), u8, bool, Reverse<(RuleSetId, i64)>);

/// Whether `key` beats the best so far. A tie keeps the earlier hit; see the mutation exclusion in
/// `.cargo/mutants.toml` for why that cannot change an answer.
fn outranks<K: Ord>(key: &K, best: &K) -> bool {
    key > best
}

/// Two hits that tie on all of it decide the same way: same precedence, same kind of pattern (an
/// exact entry is the host; suffixes of equal length matching one host are equal), scope rank and
/// effect.
fn key(hit: Hit<'_>) -> Key {
    let (precedence, scope_rank) = match hit {
        Hit::Rule(rule) => (rule.precedence.get(), rule.scope.rank()),
        Hit::Entry(_) => (Precedence::BUILT_IN, Scope::Global.rank()),
    };
    (
        precedence,
        hit.pattern().specificity(),
        scope_rank,
        hit.effect() == Effect::Deny,
        Reverse((
            hit.rule_set().unwrap_or(RuleSetId::System),
            hit.rule_id().map_or(-1, |id| id.0),
        )),
    )
}

impl RuleIndex {
    /// Indexes rule rows, set entries, the switches and who owns which workspace. All `entries`
    /// are indexed here; the store passes only System managed ones and shares the built-in part
    /// through [`RuleIndex::with_built_in`].
    pub(crate) fn new(
        rules: Vec<Rule>,
        entries: Vec<SetEntry>,
        switches: Switches,
        owners: HashMap<WorkspaceId, UserId>,
    ) -> Self {
        let mut rule_exact = HashMap::new();
        let mut rule_suffix = HashMap::new();
        for (index, rule) in rules.iter().enumerate() {
            index_pattern(&mut rule_exact, &mut rule_suffix, &rule.pattern, index);
        }
        Self {
            rules,
            rule_exact,
            rule_suffix,
            system: EntryIndex::new(entries),
            built_in: Arc::default(),
            hidden: HashSet::new(),
            switches,
            owners,
        }
    }

    /// Shares `built_in` (the entries of every built-in set) and hides the sets named in `hidden`.
    pub(crate) fn with_built_in(
        mut self,
        built_in: Arc<EntryIndex>,
        hidden: HashSet<String>,
    ) -> Self {
        self.built_in = built_in;
        self.hidden = hidden;
        self
    }

    /// The user `workspace` belongs to, or `None` when it is nobody's.
    pub(crate) fn owner_of(&self, workspace: &WorkspaceId) -> Option<UserId> {
        self.owners.get(workspace).copied()
    }

    /// Whether `set` is on for `workspace` (R-37), by its user's switches; no set is on for a
    /// workspace that is nobody's.
    pub(crate) fn is_on(&self, set: RuleSetId, workspace: &WorkspaceId) -> bool {
        self.owner_of(workspace)
            .is_some_and(|user| self.switches.is_on(user, set, workspace))
    }

    /// Whether `user` switched the entry `pattern` of the built-in `set` off (R-65).
    pub(crate) fn entry_off(&self, user: UserId, set: RuleSetId, pattern: &Pattern) -> bool {
        self.switches.entry_off(user, set, pattern)
    }

    /// `user`'s workspaces that override `set`, sorted by name.
    pub(crate) fn overrides(&self, user: UserId, set: RuleSetId) -> Vec<(WorkspaceId, bool)> {
        self.switches
            .overrides(set)
            .into_iter()
            .filter(|(workspace, _)| self.owner_of(workspace) == Some(user))
            .collect()
    }

    /// Every rule row, expired ones included until the sweeper removes them.
    pub(crate) fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// The built-in and System managed entries.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> &[SetEntry] {
        &self.system.entries
    }

    /// The shared built-in entries.
    #[cfg(test)]
    pub(crate) fn built_in(&self) -> &Arc<EntryIndex> {
        &self.built_in
    }

    /// The switches.
    pub(crate) fn switches(&self) -> &Switches {
        &self.switches
    }

    /// What decides `host` requested by `workspace` at `now`, or `None`; always `None` for a
    /// workspace that is nobody's, which has no rules (the store refuses its requests).
    ///
    /// Only the rules of the workspace's user count: their non-expired, switched-on global rules
    /// and rules of `workspace` (R-5, R-7), entries of sets they made that are switched on for
    /// `workspace` (R-37), entries of switched-on built-in sets, and System managed entries for
    /// every workspace or for `workspace`. Of those that match, R-60 decides: the highest
    /// precedence, then specificity, scope and deny over allow; a full tie goes to an own rule
    /// over a set's entry. With [`SuffixAllows::Ignore`], own suffix allows and every set allow
    /// are treated as no match (R-14, R-42).
    pub(crate) fn decide(
        &self,
        workspace: &WorkspaceId,
        host: &Host,
        now: u64,
        suffix_allows: SuffixAllows,
    ) -> Option<Hit<'_>> {
        let user = self.owner_of(workspace)?;
        self.decide_as(user, workspace, host, now, suffix_allows)
    }

    /// [`RuleIndex::decide`] for `workspace` as a workspace of `user`: for one that is not made
    /// yet, whose user is not recorded.
    pub(crate) fn decide_as(
        &self,
        user: UserId,
        workspace: &WorkspaceId,
        host: &Host,
        now: u64,
        suffix_allows: SuffixAllows,
    ) -> Option<Hit<'_>> {
        let ignore = suffix_allows == SuffixAllows::Ignore;
        let mut best: Option<(Key, Hit<'_>)> = None;
        for found in self.candidates(host) {
            let applies = match found {
                Hit::Rule(rule) => {
                    !rule.is_expired(now)
                        && rule.enabled
                        && match &rule.scope {
                            Scope::Set(set) => {
                                rule.user == user
                                    && self.switches.is_on(user, RuleSetId::User(*set), workspace)
                                    && !(ignore && rule.effect == Effect::Allow)
                            }
                            scope => {
                                rule.user == user
                                    && scope.workspace().is_none_or(|s| s == workspace)
                                    && !(ignore
                                        && rule.effect == Effect::Allow
                                        && matches!(rule.pattern, Pattern::Suffix(_)))
                            }
                        }
                }
                Hit::Entry(entry) => {
                    let on = match (&entry.set, &entry.workspace) {
                        (RuleSetId::System, None) => true,
                        (RuleSetId::System, Some(s)) => s == workspace,
                        (set, _) => self.switches.is_on(user, *set, workspace),
                    };
                    on && !(ignore && entry.effect == Effect::Allow)
                        && !self.switches.entry_off(user, entry.set, &entry.pattern)
                }
            };
            if !applies {
                continue;
            }
            let ranked = key(found);
            if best.as_ref().is_none_or(|(top, _)| outranks(&ranked, top)) {
                best = Some((ranked, found));
            }
        }
        best.map(|(_, hit)| hit)
    }

    /// Rows and entries whose pattern matches `host`: its exact entry, then every proper suffix,
    /// for own rules, then System managed entries, then the shared built-in ones.
    fn candidates<'a>(&'a self, host: &Host) -> impl Iterator<Item = Hit<'a>> + 'a {
        let rules = positions(&self.rule_exact, &self.rule_suffix, host)
            .filter_map(|index| self.rules.get(index).map(Hit::Rule));
        let built_in = self
            .built_in
            .matching(host)
            .filter(|entry| {
                !matches!(entry.set, RuleSetId::BuiltIn(slug) if self.hidden.contains(slug))
            })
            .map(Hit::Entry);
        rules
            .chain(self.system.matching(host).map(Hit::Entry))
            .chain(built_in)
    }
}

#[cfg(test)]
mod tests;
