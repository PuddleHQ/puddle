# Architecture decision records

Why Puddle is built the way it is. One file per decision, numbered in order; a later ADR that
changes an earlier one says so at the top of both ("amended by" / "Amends").

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-frontend-stack.md) | Frontend stack: no Angular; server-rendered first | accepted, amended by 0003 |
| [0002](0002-wire-format-and-types.md) | Wire format and type generation | accepted, amended by 0003 and 0004 |
| [0003](0003-product-frontend.md) | Product frontend: Svelte 5, decided on the product's requirements | accepted, amended by 0004 |
| [0004](0004-api-contract.md) | API contract: OpenAPI-first, not type-only | accepted |
| [0005](0005-no-base-image.md) | No base image of our own: run the team's own devcontainer image | accepted |
| [0006](0006-workspace-storage.md) | Workspace storage: a named msb disk volume per workspace | accepted |
| [0007](0007-one-rule-list.md) | One rule list: groups, and an explicit precedence on every rule | accepted; amends principle 2 |
| [0007](0007-login-capture-fails-open.md) | Login capture fails open: when it cannot work, the login goes through and the user is told | superseded by 0009 |
| [0008](0008-non-root-session-user.md) | Sessions run as a non-root user, `agent`, with passwordless sudo | accepted |
| [0009](0009-login-capture-fails-closed.md) | Login capture fails closed: when it cannot keep a token, the login fails and the real token never reaches the workspace | accepted |

## Writing a new ADR

Take the next number, `NNNN-short-slug.md`, with a title line `# NNNN — <decision>`, then `Date:`,
`Status:` and, if it changes an earlier ADR, `Amends:` (and add "amended by" to that one). Add a row
to the table above.

## Background material

0001–0006 were written before this repository existed. Their background material (plans,
comparisons, measurement logs) is not published; each ADR states the facts and reasons its
decision rests on.
