# 0009 — Login capture fails closed: when it cannot keep a token, the login fails and the real token never reaches the workspace

Date: 2026-11-06
Status: accepted
Amends: 0007 (supersedes it)

## Context

0007 let the real token pass through when capture could not work (credential store down, an answer
Puddle does not read, a token it cannot copy, a token bound to a key) and told the user afterwards.
The rule for credentials is stricter than 0007 allowed: a real credential never lands in the
workspace by a route Puddle can see. A token answer that Puddle has read and could not keep is
such a route. An audit of every route found four more cases that failed open without a notice:
an answer whose token sits in a field the profile does not name (a new `id_token`), a named field
that the answer repeats, a token nested in an object, and an answer too large to read.

## Decision

**When capture cannot keep a token, the login fails.** The tool gets a `502` with
`x-puddle-blocked: login_not_kept` and a one-line message that names the service and the reason;
the service's real answer is dropped unread and never reaches the workspace. The user gets a
`login_problem` notice with the same reason and what to do. This holds on HTTP/1.1 and HTTP/2.

The cases that fail the login on a successful (`2xx`) answer from a profile's token endpoint:

- the credential store did not take the token (`store_unavailable`);
- the answer is compressed, larger than a token answer, or not JSON or a form (`unexpected_answer`);
- a token cannot be copied (`unusable_token`);
- the token is bound to a key of the tool's own (`bound_token`);
- the answer carries a credential Puddle would not replace (`unnamed_token`): a text of 16
  characters or more in a field whose name says token, secret, password or key and that the profile
  does not name, at any depth; or a named field that is repeated or is not text.

An error answer, a `204` and a success answer with no token in it still pass: there is nothing to
keep. Capture is still all or nothing: if one token of an answer cannot be kept, none is replaced.

**The switch stays.** Login capture can be turned off per workspace or globally. With it off the
login is the tool's own and its real token lands in the workspace; the settings text says so.
Capture off decrypts none of a profile's hosts, so Puddle does not see the login and cannot raise
a notice at the moment of sign-in.

## Why

- A credential that Puddle saw and let through is a leak by the rule above; a notice after the
  fact does not take it back, and a refresh token stays valid for months.
- The failure is not silent and not stuck: the notice names the cause, and the user who needs the
  service in the workspace anyway has a switch with a stated cost.
- A refused sign-in is visible to the tool as an ordinary error, so tools fail the same way they
  fail on a network error.

## Consequences

- A locked credential store, or a service that changes its answer, now stops a sign-in. The user
  unlocks the store and signs in again, or turns capture off for that workspace.
- A service that adds a token-like field to its answer breaks login until the profile names it.
  This is deliberate: an unnamed token is one Puddle cannot keep.
- Logins for services with no profile are unchanged: their hosts are not decrypted, so Puddle does
  not see the answer (tracked separately).
