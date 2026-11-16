# 0007 — One rule list: groups, and an explicit precedence on every rule

Date: 2026-11-14, amended 2026-11-16 (precedence scale, no migration, overlap rules)
Status: **accepted**
Amends: principle 2 in [principles.md](../principles.md) (reworded below)

## Context

The Rules screen has four sections: the user's own rules, rules on method and path, rule sets, and
System managed. They decide by one hidden order: the most specific pattern wins, a workspace rule beats
a global one, deny beats allow, and the entries of a set only fill the gaps below the user's own
rules (unless a set's deny is more specific). Three things do not fit that order any more:

- The user wants one list sorted into groups, where a rule can be switched off, edited in place and
  moved to another group, and where groups can be imported and exported.
- Puddle should be able to ship a large block list (a tracker list) the user can switch on, and
  switch off one entry of when it blocks something a build needs. Principle 2 said Puddle ships no
  deny list.
- "Why was this refused or allowed?" has no answer on screen: the order is implicit, depends on
  the kind of list a rule sits in, and is not in the request's record.

## Decision

1. **One list, in groups.** The screen shows one list. A group organises rules and switches them
   together; it never decides an order. Moving a rule to another group changes nothing it decides.
2. **Every rule has an explicit precedence.** An integer from 1 to 9, higher wins, default 5.
   Built-in entries and System managed hosts sit at a fixed 0, below every rule the user can make.
   A higher precedence always beats a lower one, even when the lower rule is more specific, and
   that includes a rule on method and path against a host allow. Among rules of equal precedence
   the old order is the tie rule: pattern specificity, then workspace over global, then deny over
   allow. (The rules and their examples are in [the rules spec](../spec/rules.md), R-59 to R-66.)
3. **A request says why.** The record of a decided request lists the rules that matched, marks the
   winner, gives its precedence and links to each rule on the Rules screen.
4. **Overlaps are shown, conflicts are refused.** Two rules overlap when their host patterns can
   match a common host and their paths can match a common path. At the same precedence an overlap
   with the same effect is a warning that says which decides and why, and an overlap with opposite
   effects is an error: the rule that creates it is not saved. A rule that a higher rule fully
   covers is marked as such. The check works on patterns through an index, so it stays cheap.
5. **Anything may ship off.** Principle 2 is reworded: Puddle may ship rules that refuse, as long as
   they are off until the user switches them on, name themselves in every refusal, and lose to any
   rule of the user's own.

## Why precedence, and not "groups rank like free-standing rules"

The first proposal kept the hidden order and only made rules in the user's groups rank like
free-standing ones. That leaves an order nobody can read off the screen. An explicit number can be
read, shown in the request record, edited, warned about when two are equal, and extended: a later
rule on method and path that allows needs a rank against host rules, and a number gives it one.

## Consequences

- **Precedence before specificity.** A broad rule at a higher precedence now beats a more specific
  rule at a lower one. That is the point, and it is why a built-in block at 0 can never refuse
  what the user allowed. The scale is short on purpose: a single digit is easy to read and to say.
- **No migration.** Puddle has no users yet. Rules stored before precedence take the default 5,
  and an answer that changes because of it changes.
- **Rules on method and path rank like the rest.** One that blocks still checks after the host
  decision and can only refuse, but a host allow of a higher precedence beats it. At the same
  precedence a block against a host allow is a conflict, so "allow this host, block `DELETE`"
  needs the block at a higher precedence.
- **Conflicts are about what could match, not what did.** The error is broad on purpose (the
  user's choice), which makes the overlap check the cost to watch; the spec requires an index and
  a bounded incremental check, never a recompute of all pairs.
- **A conflict that exists cannot be undone by refusing it.** Switching on a group or a built-in
  entry is never blocked; both show as errors and deny wins until the user changes one.
- **The store keeps its names.** A group is a rule set in the store and the routes, and "group" on
  screen. Nesting groups, drag and drop, filters on query or body, overlap of path patterns and
  allow by path come later; nothing here depends on groups being flat.

## Alternatives considered

- **Two lists, allow and block, with copied entries.** Simpler to explain; removes sets, their
  per-workspace switches and "approve into set", copied entries stop updating with Puddle, and a
  list of 90 000 hosts cannot be held as rows. Rejected; a filter "show allows" or "show blocks"
  gives most of the simplicity inside one list.
- **Position decides (first match wins, as in classic firewalls).** Every drag would be a security
  change and every import a risk. Rejected.
- **Keep the hidden order.** No way to say why a request was decided, no way to rank a path allow.
  Rejected.
