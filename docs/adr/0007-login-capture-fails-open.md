# 0007 — Login capture fails open: when it cannot work, the login goes through and the user is told

Date: 2026-10-12
Status: accepted

## Context

A login made inside a workspace (`claude` `/login`, `gh auth login`, Copilot CLI) ends with the tool
calling a token endpoint through Puddle's proxy. Puddle captures it: the real tokens stay on the
host, in the operating system's credential store, and the workspace gets stand-ins that have the
tokens' shape and are swapped back only toward that service's hosts. This is what keeps a
prompt-injected agent from copying the most valuable secret in the workspace, a refresh token that
works from anywhere for months.

Capture can fail at the moment it is needed. The credential store may be locked or missing, the
service's answer may be in a form Puddle does not read (compressed, larger than a token answer, a
new shape), a token may have a length or character a stand-in cannot copy, or the service may tie
its tokens to a key of the tool's own (DPoP), which no stand-in can stand in for. Puddle has to
answer the token request one way or the other.

## Decision

**When capture cannot work, the real answer passes through unchanged, so the real token reaches
the workspace, and the user is told afterwards** with a notice that names the service, the
workspace and the reason (a `login_problem` event; the notice says the login works but an agent in
the workspace could copy it, and what to do about it). Capture is all or nothing: if one token of
an answer cannot be kept, none is replaced.

The same applies to the stand-ins left behind when capture is switched off for a workspace that
already holds captured logins: they are no longer swapped, so the services refuse them. Puddle says
so at the workspace's next start (a `capture_off` notice) instead of leaving the tool with a bare
401. A problem Puddle has no name for is reported as `unknown`, which claims nothing about the login.

## Why

- **The sandbox must not be the reason a login fails** (principle 1). Capture is a protection Puddle
  adds on top of what a dev container does today. A locked keyring, or a service that changes its
  answer's format, would otherwise stop a user from signing in at all, with Puddle as the cause and
  nothing the user can do inside the workspace about it. A sandbox that breaks logins gets
  switched off, which protects less than a capture that sometimes cannot work.
- **The token is the user's own, and they put it there.** Without capture the workspace would hold
  the same token; failing open returns the user to that baseline, not below it. It is not one of
  Puddle's credentials, which stay out of the workspace regardless (principle 4).
- **The failure is not silent** (principle 6). The notice says the login is not protected and names
  the way out (make the credential store available and sign in again; sign in again for the other
  kinds). A trade-off mentioned only in these docs would not be enough, which is why the notice is
  part of the decision.

## The trade-off against principle 4

Principle 4 says a check that cannot be completed refuses that connection and never lets it
through, and that the workspace never holds a key. Failing open is a deliberate, narrow exception:

- It lets a real token into a workspace that is untrusted, for the time until the user acts on the
  notice. An agent in the workspace can read and send it in that window, and a refresh token stays
  valid after the window.
- It is limited to a login the user is making inside the workspace on purpose, for a service on
  Puddle's built-in list. It does not apply to anything Puddle itself holds, or to any connection
  check (rules, the network guards, termination of other hosts), which still refuse when they cannot
  complete.
- The decision is the user's to weaken, not Puddle's to make quietly: the user is told every time,
  per service, and can turn capture off per workspace or globally.

## Consequences

- A user who needs a refusal (a login that must never reach the workspace unprotected) does not have
  it today. A "refuse the login" setting is the way to add it, and is built if someone asks; it
  would turn this exception into an opt-in strictness rather than change the default.
- Capture failures are visible but not blocking, so the protection is only as good as the user's
  reading of the notice. The notice appears on every screen and names the fix.
- Logins captured while capture was on stay in the credential store when capture is switched off,
  so switching it back on restores them; until then the notice for them repeats at each start.

## Alternatives considered

| Option | Why not |
|---|---|
| **Refuse the login when capture fails** | The tool fails for a reason it cannot see and the user cannot fix from the workspace: a locked keyring or a server-side format change makes sign-in impossible. Breaks principle 1 for a protection that was never there before |
| **Fail open without telling the user** | Looks protected and is not; breaks principle 6 |
| **Make refusing the default, failing open the opt-in** | Same breakage as the first row, on every machine whose credential store is not available |

## Revisit if

- Users ask for a strict mode (add the setting above).
- A service starts to bind every token to a client key, making fail-open the normal path rather than
  the exception.
- Puddle can refresh logins on the host by itself, so a workspace never needs the real token at all.
