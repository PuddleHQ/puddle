# Rules spec: firewall rules, pending requests, audit

Status: draft for the `puddle-store` rules engine and SQLite store, 2026-10-06.
Written fresh from Puddle's own design decisions and threat model. No upstream Huddle rules, store
or test code was used.

Every numbered rule (`R-n`) gets at least one named test in `puddle-store` (`r07_expired_rule_never_matches`, etc.).
Defaults marked *(default)* are reversible choices.

## 1. Model

A **request** is what the proxy asks about: `(workspace_id, host, port)`. Every `workspace_id` in
this document, and every workspace scope of a rule, is the workspace's id, which never changes; the
name the user gave the workspace is display text and keys nothing (a rename changes no rule). The
workspace id comes from
the route the connection arrived on, never from anything the guest says. `host` is a
normalised name (lowercase, IDNA to ASCII, LDH labels, no trailing dot, at most 253 characters) or
a canonical IP literal (IPv4 dotted quad, IPv6 per RFC 5952). The proxy normalises; the engine only
accepts the normalised type and never parses raw input.

A **rule** says what happens to matching requests:

| Field | Meaning |
|---|---|
| `id` | `i64`, never reused (SQLite `AUTOINCREMENT`) |
| `scope` | `global` or `workspace` (with `workspace_id`) |
| `pattern` | `exact` host or IP, or `suffix` (stored as `.example.com`) |
| `effect` | `allow` or `deny` |
| `expires_at` | epoch ms, or `null` for permanent |
| `created_at`, `created_by` | `ts`; actor `cli`, `ui` or `api` |
| `source_pending_id` | the pending row it was approved from, or `null` |

A **pending row** is a request that matched no rule (§3). Rules and pending rows live in SQLite;
the audit (§5) is a SQLite table that is also served as a JSONL stream.

Credential bindings and the local-destination toggles are separate settings, not rules:
a binding never allows a host, and a toggle never allows a destination (R-14).

## 2. Matching

- **R-1 Nothing is pre-decided.** A fresh install has no rules, allow or deny. Every request
  with no matching rule becomes pending (§3). Every built-in group, one that allows and one that
  blocks alike, ships switched off (R-36); no built-in rule decides anything until the user
  switches it on. The one thing Puddle allows by itself is the short System managed list, derived
  from the user's own setup choices and shown with a reason for every host (R-40, R-41).
  *Note added 2026-10-08, reworded 2026-11-14.*
- **R-2 Exact rules** match the identical normalised host or IP literal, on any port. Port rules are
  later; the port is still recorded on pending rows and in the audit.
- **R-3 Suffix rules** `.example.com` match every name ending in `.example.com` at any depth, and
  not `example.com` itself *(default)*. Input `*.example.com` is accepted and stored as
  `.example.com`. A suffix rule never matches an IP literal.
- **R-4 A suffix must be longer than a public suffix.** `.com`, `.co.uk` and other entries of the
  bundled public suffix list (ICANN and private sections) are refused as suffix patterns.
- **R-5 Applicable rules** for a request: every non-expired `global` rule plus every non-expired
  `workspace` rule of that request's workspace. Another workspace's rules never apply.
- **R-6 Precedence, most specific wins** among rules of the same precedence (R-59, R-60): (1) exact beats suffix, and a longer suffix beats a shorter
  one; (2) at equal pattern specificity, `workspace` beats `global`; (3) at equal scope, `deny` beats
  `allow` *(default)*. So a global deny on `.example.com` can be overridden for one workspace by a
  workspace allow on `.example.com` or an exact allow, but never by a broader pattern.
- **R-7 Expiry is checked at decision time.** A rule with `expires_at <= now` (host clock, epoch ms)
  never matches, whether or not the sweeper (§4) has run. The guest's clock plays no part.
- **R-8 Changes apply on the next request**, with no workspace or proxy restart: a rule change commits
  to SQLite first, then replaces the engine's in-memory rule set atomically. A decision never sees a
  half-applied change. Connections already open when a rule is deleted or expires are not cut
  *(default)*; the next connection is decided anew.
- **R-9 The decision says how it matched:** `allow { rule_id, pattern }`, `deny { rule_id }` or
  `pending { pending_id, outcome }` (`outcome`: `new`, `repeat`, `unlisted`: R-45). The proxy needs the
  pattern kind for R-14. A rule set's entry decides as `set_allow { set, rule_id?, pattern }` or
  `set_deny { set, rule_id?, pattern }` (§7; `rule_id` is absent for a built-in entry). Once
  precedence is built (R-59), every decision that a rule made also carries the winning
  `precedence` and the rules that matched (R-62).

## 3. Pending requests

States: `requested → allowed | denied | expired`. The three end states are final. A row only
expires when its workspace is deleted (R-21): a waiting request never lapses by age (R-46).

- **R-10 Unmatched requests are refused for now and recorded.** The proxy answers 403 at once
  (fail fast, the client retries), but the request is not denied: its row stays open until the user
  decides (R-46), and the answer says so (R-47). The row keeps `first_seen`/`last_seen` so
  parking the connection can be added later without a schema change. The name is never resolved
  before a rule allows it.
- **R-11 Dedupe on `(workspace_id, host, port)`:** at most one `requested` row per key (a partial unique
  index). A repeat updates `last_seen` and increments `attempts` on the existing row; it creates
  nothing. A request after the row ended (for example the allow it created later expired) opens a
  new row with a new id.
- **R-12 Pending rows are immutable:** `id`, `workspace_id`, `host`, `port` and
  `first_seen` never change after insert; ids are never reused. Only `last_seen`, `attempts`,
  `state`, `decided_at`, `decided_by` and `rule_id` change, and `state` only along the arrows above.
- **R-44 Volume never refuses a request.** A request no rule matches is never denied, dropped or
  hidden because its workspace asks about many hosts: no rate limit slows new rows, and nothing is
  written to the audit as "held back". It is recorded and waits like any other.
- **R-45 A flood is bounded without refusing.** A hostile workspace must not grow the store
  without limit. A workspace has at most 1000 *(default)* open rows. A new host asked about past
  that gets no row of its own (outcome `unlisted`), and is answered exactly like a waiting request
  (R-47): it is never denied, and it gets a row when it asks again once the user has answered
  some. Repeats of an open row (R-11) never count against the cap. Other workspaces have their own.
  `GET /api/inbox` reports the cap (`open_limit`), and the inbox says when a workspace has that many
  waiting, so a host that is not listed is never a silent one.
- **R-47 The refusal says the request is waiting.** The 403 body says in words that the host is
  waiting for approval in Puddle and that the tool should retry once it is approved; the headers
  keep `x-puddle-decision: pending` and, when a row exists, `x-puddle-pending: <id>`. For an
  `unlisted` request (R-45) the body says instead that the host is not listed for approval yet
  because Puddle holds more waiting requests for the workspace than it lists, and that answering some
  lists this one on the next try. The connection
  is not held open ("parked"); that stays a later option.
- **R-14 Local destinations** (the proxy applies this after resolving an allowed name; the engine supplies
  the match): an address in a local category with its toggle off is blocked and the block names
  the toggle, with no pending row. With the toggle on, only an `exact` allow
  counts: of the name, or of the resolved address itself (an exact IP rule admits that address,
  and only it). A suffix allow is treated as no match, and if no address is admitted the request
  goes pending for the exact name, unless the "wildcards reach local addresses"
  setting is on (global default off, per-workspace override *(default)*). The IP check only looks
  up rules (`Policy::lookup`); it never writes a pending row for the address. Puddle's own
  endpoints are blocked whatever rules or toggles say, and never become pending. So is SSH to a
  Git host's name that Puddle does not serve (reason `ssh_unsupported`, R-24): the proxy recognises
  SSH by the client's identification line on any port and refuses it before any rule is asked, so no
  rule allows it and it never goes pending. SSH to any other name is an ordinary connection: the
  rules decide it as for any `CONNECT` (R-2, so a rule for the name covers every port), a request
  with no rule goes pending as that host and port, and an allowed one is spliced end to end. UDP other than DNS is refused the same way, inside the
  workspace, before it leaves; the workspace's agent reports where each refused datagram was going
  and the proxy records it as `blocked` with reason `udp_unsupported` (never pending, never
  approvable; one record per destination at most every ten seconds).
  *Changed 2026-10-06: the exact-IP case was added, following the principle that a local destination
  needs an exact name or IP entry; before, only an exact name rule or an approval counted.*
- **Company-network destinations** are a sixth category next to the local ones, with its own toggle
  (global, with a per-workspace override, off by default). A resolved address is in the category when
  the host sees one of these signals: the system proxy configuration answers direct for the URL while
  the same configuration sends other URLs through a proxy (`pac_direct`); the name falls under a DNS
  suffix the host is configured with, as its primary or connection-specific suffix, on the search list
  or in a name-resolution policy namespace (`dns_suffix`); the address is in `100.64.0.0/10` while a
  private-access network client is present (`private_access_cgnat`); or the address is in RFC 1918
  space and is routed through a VPN interface rather than the physical LAN (`vpn_route`). An address
  that also fits local/private network counts as company network. An address on the physical LAN's own
  subnet stays local/private network, and `100.64.0.0/10` with no private-access client stays there
  too. Loopback, link-local, metadata, special-use and Puddle's own endpoints are never company
  network. A signal the host cannot read counts as no signal, so the address stays local/private
  network: the company toggle never opens what the private toggle guards. With the toggle off, a
  company-network destination is blocked before the rules are asked, with no pending row, and the
  block names the signal and the switch. With it on, the rules of this section apply unchanged: only an
  exact allow counts, and a suffix allow reaches the destination only when "wildcards reach local
  addresses" is on. The toggle allows nothing by itself. On a proxied route the check is made on the
  address the host resolves for the name, with a short timeout. An answer that is public, missing or
  late does not deny, because the company proxy may be the only resolver. The reading of the
  physical LAN has a third answer, "unread": while the host cannot read its interfaces it cannot
  rule out that an address is on the LAN, so the address stays local/private (a plain "no" there
  would move a home-LAN address into the category). Image pulls use the same verdict, with the
  workspace's name block, and have no rules, so an address whose toggle is on is reachable.
  **Status:** on Windows the host reads all four signals, read-only: the system's proxy script
  (asked in the background, so the first request to a name still reads as plain private network and
  the next one as company network), the adapters and IPv4 routes, and the DNS suffixes and
  namespaces. A client counts as private-access when an adapter that is up carries the name of
  Zscaler, Netskope, Cloudflare WARP, Tailscale or Microsoft's private access. On Linux and macOS
  the host reads none of them, so the category is empty there and the toggle changes nothing; the
  Settings text says so.
  Two exceptions belong to the category and have no switch of their own; each use is a
  `connection` record. (1) A plain-HTTP `GET` or `HEAD` of a certificate distribution point or
  authority-information-access URL (CRL, issuer certificate, OCSP responder) named in a root the
  host synced is allowed with no rule and with the toggle off, when the host in the URL resolves
  only to company-network or private addresses (reason `pki_distribution_point`). The URLs are read
  from the synced roots at start; the exception covers that exact host, port and path (a request
  path below the URL counts as a directory, for an OCSP `GET`), never another method, host, port or
  path, and never loopback, link-local, metadata or a public address. (2) SSH to
  `ssh.dev.azure.com` port 22 stays refused (`ssh_unsupported`); when the host's own probe of
  that port fails, the refusal says the company network blocks it and that HTTPS remotes work with
  Puddle's credentials, instead of only pointing at HTTPS.
  *Added 2026-11-05; the unread LAN answer 2026-11-13; Windows readers 2026-11-13.*
- **R-27 An exact IP deny wins for a resolved address** (the proxy applies this to every address an
  allowed name resolves to, before R-14). The proxy looks up each address's own rules
  (`Policy::lookup`, R-5 to R-7 as for a request to that literal): if they deny it, the address is
  never used, whatever allows the name (exact, suffix, or an approval) and whether it is local or
  public. The remaining addresses go on to R-14. If every address is denied, the request is denied
  with the IP rule's id (`x-puddle-rule`, and `rule_id` with `resolved_ip` in the `connection`
  record); no pending row is written, because approving the name can't change it. A failed lookup
  refuses the request (fail closed). A request for an IP literal was already decided as that
  address. *(default)*: an address's own rules use R-6 precedence, so a workspace allow of the
  address beats a global deny of it. Numbered R-27 to keep the other numbers (and their test
  names) stable. *Added 2026-10-06: before, an IP deny was checked only on the wildcard
  path, so an exact name allow or an approval reached a denied address (firewall model,
  2026-10-04: false allows are very bad).*
- **R-15 Approve and deny** take a row id and four choices: effect (`allow`/`deny`), scope (`workspace`,
  the default, or `global`), pattern (`exact`, the default, or a suffix of the row's host that passes
  R-4) and expiry (permanent, the default, or a duration). This covers the inbox's four outcomes
  (allow/deny × this workspace/everyone). Defaults are never widened implicitly. Instead of a scope,
  the rule can go into a rule set the user made (R-38).
- **R-16 A decision is one transaction:** create the rule, set the row to `allowed`/`denied` with
  `rule_id`, `decided_at` and `decided_by`, and close every other `requested` row the new rule now
  decides (same workspace for a workspace rule, any workspace for a global one) the same way.
- **R-17 Stale ids are refused.** Approving or denying an unknown id or a row not in `requested`
  fails with an error naming the row's current state; nothing changes. A successful call returns
  the row as decided (workspace, host, port) and the rule created, so the CLI and UI can echo exactly
  what was approved.
- **R-18 The inbox groups by registrable domain** (public suffix list, as R-4) for display; grouping
  is derived, not stored, and never decides anything.

## 4. Sweeper

One background task, every 60 s *(default)* and at startup. (R-20 is retired: see R-46.)

- **R-19** deletes rules with `expires_at <= now` and writes a `rule_expired` audit record holding
  the whole rule. Correctness never depends on it (R-7).
- **R-46 Requests never expire.** The sweeper never closes a `requested` row: it waits until the
  user approves or denies it. Rule expiry is separate and stays (R-7, R-19). A request after its
  rule lapsed opens a new row (R-11).
- **R-21** on workspace deletion (not stop), deletes that workspace's rules (`rule_deleted`, reason
  `workspace_deleted`) and expires its open rows, in the same transaction as the deletion.
- **R-22** enforces the audit cap (R-26).

Sweeper work never holds a lock that a decision waits on for more than one short transaction.

## 5. Audit

Every decision, rule change and pending change is one record: a row in the SQLite `audit` table,
readable as JSONL (one record per line).

- **R-23 Serialised with `serde_json` only**, from one internally tagged enum
  (`"type": "..."`), per ADR 0002: `snake_case` fields, `ts` in epoch ms, `null` always written,
  never `untagged`, additive changes only. Control characters in any string come out escaped, so
  every line parses with `jq`. Lines are never rewritten: records written before the rename of
  sandbox to workspace carry `sandbox_id`, a rule scope `sandbox`, an origin `sandbox` and a
  reason `sandbox_deleted`, and read as `workspace_id`, `workspace`, `workspace` and
  `workspace_deleted`. The API adds `workspace_name` to each entry of the audit page: the name the
  workspace had when the record was written (`null` for a record written before names were kept, and
  for one about no workspace; a reader then shows the id), so a record of a renamed or deleted
  workspace still reads as the user knew it. The line itself is unchanged.
- **R-24 Record types** in `puddle-store`: `connection` (written by the proxy: `workspace_id`, `host`, `port`,
  `resolved_ip`, `category` (`company_network` for a destination in that category, `null` otherwise and in older records), `signal` (`pac_direct`, `dns_suffix`, `private_access_cgnat` or `vpn_route`: the signal that put it there, `null` when none is known), `decision` (`allow`, `deny`, `pending`, `blocked`), `outcome` (how reaching the destination went after an allow: `connected`, `upstream_refused`, `connect_failed`, `timeout`; `null` where nothing was attempted and in older records), `reason` (`rule`, `no_rule`,
  `toggle:<category>` (`toggle:company_network` for the company network), `puddle_endpoint`, `ssh_unsupported`, `udp_unsupported`, `local_address`, `settings_unreadable`,
  `pki_distribution_point`, `policy_unavailable`, `request_rule` (R-54), `sni_mismatch`, `token_endpoint_spliced`, `guest_tls_rejected`, `certificate_unavailable`, `limit:<code>`, `connection_limit`, `bad_git_path` (a Git path Puddle does not read, over HTTPS or SSH), `ssh_command_unsupported` (an SSH command that is not `git-upload-pack` or `git-receive-pack`), `ssh_channel_refused` (an SSH channel request Puddle does not allow), `suppressed`, `merged`, ...), `rule_id`, `rule_set` (the set whose entry decided, R-43), `request_rule` (the rule on method and path that refused the request, R-54), `refused_address` and `refused_reason` (the first address of the answer that an IP rule denied (`ip_rule`), else the first the address guard refused, and its reason code; visibility only, the connection still goes to the allowed addresses; `null` when none was refused and in older records), `pending_id`, `binding_id`, `injected`, `unswapped_stand_ins` (the ids of the secret stand-ins that
  went out unchanged: to a host outside their hosts, or in the path or query; also `stale-stand-in:secret:<name>` and
  `stale-stand-in:github` for a value shaped like a stand-in that the workspace does not hold),
  `scrubbed_stand_ins` (the ids of the stand-ins whose real values an answer from one of their
  hosts held and that went to the workspace as the stand-in instead; empty in older records; an
  answer in a content encoding Puddle cannot read is refused with reason `answer_encoding`, one cut
  off because it held a real value in a form that cannot be replaced has reason `answer_withheld`),
  `foreign_credentials` (the kinds of credential a request carried that Puddle did not add or swap:
  `stale_stand_in`, `github_token`, `npm_token`, `gitlab_token`, `azure_devops_token`,
  `git_authorization`; each once, never a value; empty in older records and when the warning is off),
  `answer_credentials` (the kinds of token an answer held that Puddle does not hold, read as the
  workspace gets it: `github_token`, `github_fine_grained_token`, `gitlab_token`, `npm_token`,
  `azure_devops_token`, `anthropic_key`, `openai_key`, `jwt_with_refresh_token`; each once, never a
  value; the same setting turns it off; empty in older records),
  `method` and `path` on terminated hosts, plain-HTTP requests and `CONNECT` tunnels that carry
  plain HTTP/1.x only, `bytes_up`, `bytes_down`), `pending_created`, `pending_decided`, `pending_expired` (only when a workspace is deleted),
  `rule_created`, `rule_updated`, `rule_deleted`,
  `rule_expired` (with the full rule), `request_rule_created`, `request_rule_deleted` (R-49, the rule on method and path with
  its host, methods and path, and for a deletion why and by whom), `workspace_op` (R-56), `audit_trimmed` (`deleted_records`, `oldest_ts_kept`), and
  the rule set records of R-43. Records written by earlier versions still read: `pending_suppressed`
  (`workspace_id`, `count`) and `pending_expired` with reason `stale`; nothing writes them any more.
  On a decrypted host whose client refuses Puddle's certificate (it does not
  trust the workspace's CA) the decision stays what the rule said and the reason is `guest_tls_rejected`; when Puddle's own CA cannot make
  the certificate for the host (an expired CA, a failure while issuing), the handshake fails on Puddle's side, the decision stays too and the
  reason is `certificate_unavailable`.
  When a limit of the proxy ended a decrypted connection or cut one of its streams or pipes (a header
  block left open, a control-frame flood, a stream with no frame for an hour, a peer that ignored the
  shutdown, a silent WebSocket) the decision stays too and the reason is `limit:<code>`
  (`header_block_too_slow`, `control_frame_flood`, `stream_idle`, `shutdown_ignored`,
  `websocket_idle`; the first limit that fires on a connection is the one recorded); a request refused for the workspace's HTTP/2 stream budget is `blocked` with
  `stream_limit`.
  The proxy writes one `connection` record per request whose destination it parsed, when the
  connection ends. A guest connection that carries several plain-HTTP requests (keep-alive) writes
  one per request, each decided on its own and written once its answer has gone, with the bytes
  moved while it ran. A request refused before that (bad request, head too large or too slow) has no
  destination and only goes to the log. A connection over the workspace's limit is read for up to
  5 s so its destination is known: it gets a `503` saying why and a `blocked` record with reason
  `connection_limit`, and the rules are not asked. `resolved_ip`
  is the address connected to (`null` if none was), and the bytes are counted on the guest side,
  proxy responses included.
  A `CONNECT` tunnel is decided like any request (rules see `(workspace, host, port)`, R-2), so
  `CONNECT host:80` and `GET http://host/` get the same decision and pending row. Node `fetch` and
  Yarn Berry send `http://` URLs that way. When a tunnel's first bytes are an HTTP/1.x request line,
  its record carries that request's `method` and `path` (first request only); the bytes are
  relayed unchanged. *Added 2026-10-06.*
  `upstream` names the company-proxy hop that carried the connection (`DIRECT` or `PROXY host:port`,
  never credentials; `null` when no upstream route is configured or nothing connected), and when
  it is a proxy hop `resolved_ip` is the address sent to the proxy, or `null` if the proxy was told
  the name (or this host could not resolve it). *Added 2026-10-07 (additive).*
  `origin` says whose connection it is: `workspace` (a workspace's, the default) or `puddle` (Puddle's
  own, made on the host with no workspace: image pulls through the pull proxy). A `puddle` record has
  `workspace_id: null`, `reason: puddle_request` when the address guard let it through and the pull connected
  (`puddle_request_failed:unreachable` or `:timeout` when it could not resolve or connect; a block
  reason when the guard refused it), no rule or pending row, and carries `upstream`, `resolved_ip` and the
  bytes like any other. The pull proxy writes one per pull whose destination it parsed; a request
  refused before that (no or wrong token) is only logged. Records written before `origin`
  existed have no such field and read as `workspace`. *Added 2026-10-07 (additive).*
- **R-25 No secrets.** Never header values, credential material, query strings or request bodies;
  credentials appear only as `binding_id` and `injected: true|false`. Every audit struct has a test
  that serialises it with canary values in every secret-bearing input and asserts the canary is
  absent.
- **R-26 Size caps.** One line is at most 4 KiB: `path` is cut to 1 KiB *(default)* with
  `path_truncated: true`, and any other oversize string field is cut the same way. Total audit is
  capped at 256 MiB *(default)*; the sweeper deletes the oldest records first and writes one
  `audit_trimmed` record per trim. Per workspace, `connection` records are limited to 200 per second
  *(default)*; the excess is counted and written as one `connection` record with
  `reason: suppressed` and a `count` per second. Puddle's own connections (`origin: puddle`) share one limit of the same size.
  Per workspace, the `connection` records written while a request waited (decision `pending`) are also
  capped at 5000 *(default)*: the sweeper deletes the oldest ones past it, hosts and all, and writes one
  `connection` record with `reason: merged` and the number deleted as `count`. A flood of requests nobody answers cannot
  fill the audit and push real history out, and nothing is refused (R-44, R-45).
- **R-56 Workspace operations are audited.** A `workspace_op` record says who asked (`actor`: `cli`,
  `ui`, `api` or `system`), what was done (`op`), to which workspace (`workspace_id`, `null` for an
  identity, which belongs to none), how it ended (`outcome`: `succeeded` or `failed`) and a short
  plain sentence (`detail`, `null` when there is nothing to add). The `op` values are `create`,
  `delete`, `rename`, `start`, `stop`, `identity_created`, `identity_updated`, `identity_deleted`,
  `identity_default_set`, `identities_changed`, `identities_reordered`, `repo_added`, `repo_switched`, `repo_removed`, `root_disk_grown` (a workspace's root disk made bigger, or the try failed; `detail` gives both sizes and how it took effect) and
  `git_repaired` (one record, actor `system`, for each zero-filled or corrupt git file Puddle rebuilt
  at boot; `detail` names the file and what was done).
  A create or a delete is written whether it worked or not; a start or a stop only when it failed
  (a start that worked shows in the workspace's state); a rename when it worked, or when the
  workspace list could not be saved; the identity and repository changes, a new order of the identities included, when they worked. A call
  Puddle refuses before it changes anything (a name taken, a workspace that is not stopped) is the
  caller's error and writes nothing. A delete of a workspace whose volume was already gone says so in
  `detail`, and a failure's `detail` is the reason the user was shown. `detail` holds names, labels
  and ids only, never a secret (R-25). The record is additive: logs written before it read as before.
  The record is filtered like any other (R-28): `type=workspace_op`, `workspace`, and
  `outcome=succeeded|failed`, which only this record has. A record that cannot be written is logged
  and dropped, and the operation it describes stands.

- **R-28 Reading the audit with filters.** The API filters on the server, on stored columns and
  indexes (never by parsing JSON): `workspace`, `type`, `outcome`, `host_contains` (case-folded
  substring), `q` (the search box, below), `from` (inclusive) and `to` (exclusive) as epoch ms. All
  set filters must match.
  `host` is the record's host, or a rule record's pattern. `outcome` (`allow`, `deny`, `pending`,
  `blocked`, `expired`, `succeeded`, `failed`) exists for `connection` (its `decision`), `pending_created` (`pending`),
  `pending_decided` (`allow` or `deny`), `pending_expired` (`expired`) and `workspace_op` (`succeeded` or `failed`); every other record has
  none and never matches an `outcome` filter. `origin` (`workspace` or `puddle`) matches `connection`
  records only (a Puddle record is one without a workspace); every other record has no origin and
  never matches. *`origin` added 2026-10-07.* `q` is a case-folded (ASCII) substring over one stored
  text column that holds, for a `connection`, its host, method, path and reason, for a rule on
  method and path its host and path, and for any other record its host (a rule's pattern); a record
  with none of these never matches. It is a plain substring, not a pattern, and a control character
  in it is refused, so one field's end and the next one's start can't match together. *`q` added
  2026-11-05.* Pages are at most 500 records: newest first, paged
  back with `before`, or oldest first from `after` to follow the tail.
- **R-29 Events.** After each commit the store emits, per change: `pending_opened` (a new open
  row), `pending_updated` (a repeat: `attempts`, `last_seen`), `pending_closed` (decided by a user
  or a rule, or expired with its workspace: `state`, `rule_id`), `rules_changed` (any rule created, changed,
  deleted or expired, any rule set made, renamed, deleted or switched, and any change of System
  managed) and `audit_appended` (once per commit that wrote audit records, with the
  newest id). Events carry ids and counts, not decisions: a client that missed some refetches.
  A failed change emits nothing. `network_changed` (with the new network epoch) is not a store
  event: Puddle sends it when the network or the system's proxy settings change, and a client
  refetches `GET /api/network-health`.

## 6. Name lookups from a guest that ignores the proxy settings

Some tools ignore `HTTPS_PROXY`: they resolve a name and connect to the answer. The guest agent
runs a stub DNS server for them; it answers each address query with a *stand-in address* from
`198.18.0.0/15` and asks the host about the name first, over the agent's `resolve` stream. The
connection to the stand-in is redirected to the agent, which sends `CONNECT name:port` like any
other client, so R-1 to R-14 apply to it unchanged. These rules say what the host answers to the
lookup. They are tested in `puddle-proxy` (`resolve`) and `puddle-e2e` (`stub_dns`), not in the
store.

- **R-30 A lookup writes nothing, except the `SRV` requests of R-34 and R-36.** It asks the rules engine
  without recording: no pending row, no audit record, no counter. The pending row and the deny
  come from the connection that follows (R-10), by name.
- **R-31 A name that no rule allows is not looked up.** An unmatched name, a denied name and a name
  blocked by itself (R-14: a toggle that is off, one of Puddle's endpoints) all get a stand-in
  and cause **no** lookup on any resolver, so DNS is not a way out and a guest asking for many
  random names causes no resolver traffic. The connection that follows is refused with the
  usual reason. Record types other than `A` (`TXT`, `SRV`, `MX`) of such a name are answered
  with no data; R-36 adds one request for an `SRV` query of a name no rule matches.
- **R-32 An allowed name is resolved once, on the host.** If it resolves, the guest gets a
  stand-in. If the host's resolver says there is no such name, the guest gets `NXDOMAIN`, except
  under R-33; if the lookup itself fails (no resolver answered, the network is down) it gets
  `SERVFAIL` and the host logs why, never a cached "no such name". If every address it resolves to is refused by R-14, the guest still gets a stand-in:
  the connection says which toggle would allow it. The addresses never reach the guest, and the
  lookup does not constrain the connection, which resolves and checks again (R-14, R-27).
  A name under `.invalid` (or the bare `invalid`) is never looked up: the host answers "no such name"
  at once, as RFC 6761 allows, even where a private DNS would resolve it. It is treated like any
  other name that does not exist, so R-33 still applies.
- **R-33 A name the host can't resolve goes to the company proxy by name, when one is in the
  route.** If an upstream proxy is in the route and sending unresolvable names to it is on (the
  default), an allowed name the host can't resolve, or whose lookup fails or times out, gets a
  stand-in too: on some networks only the proxy resolves internet names, and the proxy decides when
  the connection arrives. With no upstream proxy, or with that setting off, it is `NXDOMAIN` for a
  name that does not exist and `SERVFAIL` for a lookup that failed or timed out.
- **R-34 `SRV`, `TXT` and `MX` are looked up for allowed names only.** The rules decide the name
  without its leading service labels (`_mongodb._tcp.db.example.net` is decided as
  `db.example.net`), since a service label is not part of a host the user approves. The targets of
  the `SRV` records kept (below) and of `MX` records get stand-ins of their own in the answer. At
  most 16 records and 8 KiB of text are passed on; records left out (another type, a target that
  is not a plain host name, more than the cap) are said in the log.
  An `SRV` answer keeps only the records whose target a rule allows, because a client can't fall
  back from a target the proxy refuses: the connection to a stand-in is accepted in the guest
  before the proxy decides, so the refusal arrives as a reset after a successful connect. apt asks
  `_http._tcp.deb.debian.org` first, and its record points at `debian.map.fastlydns.net`; with that
  record in the answer, a user who allowed `deb.debian.org` would see `apt-get update` fail when
  apt runs without its proxy file. With the record left out, apt connects to `deb.debian.org`
  as it does when there are no records. A target no rule matches also raises one request for
  itself, at the record's port, so a client that has no fallback (`mongodb+srv://`) shows up for
  approval; a denied or blocked target (R-14) raises nothing. When every record is left out the
  answer is no data. Each record left out is a line in the host log naming the target and why
  (no rule, a deny rule, the block's reason, or not a host name).
- **R-35 A lookup is bounded.** The name must be a plain host name (labels of letters, digits, `-`
  and `_`, at most 253 characters) or the answer is "no such name" without a lookup. A workspace runs
  at most 128 host lookups at once; one over that waits up to one lookup time (3 s) for a free slot
  and is then "unavailable" (`SERVFAIL` in the guest). The guest's stub is bounded the same way (128 asks, one lookup time to wait); a query that waited its whole time for a slot was never tried, so the stub drops it and the guest's resolver asks again instead of taking a failure for the name. One agent session has at most 64 lookups
  open (the agent keeps four sessions, so its 128 lookups in flight fit). Each lookup has a
  timeout. An unreadable rules engine is "unavailable", never "not allowed". A burst of 500 distinct
  names asked at once, as a package manager's cold start can, must all be answered (standing test
  `a_burst_of_five_hundred_distinct_names_all_resolve_for_a_client_with_glibc_defaults`).
- **R-36 An `SRV` query for a name no rule matches raises one request for its base name.** Clients
  such as `mongodb+srv://` ask for `SRV` first and give up on "no records" without ever
  connecting, so the user would never see them. The query `_mongodb._tcp.cluster.example.net`
  therefore opens one pending request for `cluster.example.net` (the leading service labels are
  stripped, as in R-34), with port 0 because the service's port is not known yet. It is
  deduplicated, bounded and shown like any other pending request (R-11, R-45), and the answer is
  no data until the user decides; once the name is allowed the same query is looked up (R-34).
  It applies to `SRV` only: `TXT`, `MX` and address queries still raise nothing. A name a deny
  rule matches, a name blocked by itself (R-14) and a name that is not a plain host name raise
  nothing.

The stub answers names that can never be a connect target itself (reverse zones, names that end in
a number) with `NXDOMAIN`. Every other name, including single labels and zones such as `.local` or
`.internal`, is decided by the rules like any other, so a company's intranet names work once
allowed. Every other record type (`AAAA`, `HTTPS`, ...) gets no data, so dual-stack
clients use the stand-in at once.

## 7. Rule sets and System managed

A **rule set** is a named bundle of entries switched on or off as one. *Built-in* sets ship with
Puddle; the user makes *their own*. **System managed** is a separate, read-only list: the hosts
Puddle allows because of choices the user made (which browser editor server, direct SSH), each
with its reason. The user's own global and workspace rules (§1) are called *own rules* below. These
rules are tested in `puddle-store` (`rule_sets_spec`, the engine's property tests), the proxy and
the API. *Added 2026-10-08.*

- **R-36 Built-in groups ship off, and update with Puddle.** A built-in set (shown as a *group*,
  R-59) is data in Puddle (an id `builtin:<slug>`, a name, a description, entries with a note each
  and an effect each), checked at build time against R-4 and for duplicates. It is read-only and
  never copied into the database, so an update of Puddle updates it. A built-in group may allow, or
  block, or hold both. **Every built-in group ships switched off**, whatever it does: Puddle never
  refuses a request on its own, only because the user switched a group on (principle 2). A
  refusal by a built-in entry names the group and the entry (R-62, R-53), and the user can switch
  that one entry off (R-65) or the group. A built-in entry decides at the fixed lowest precedence
  (R-59), so an own rule that matches the host always beats it. When an update changes a group's
  entries, the store writes one `rule_set_changed` record (`added`, `removed`) when it opens, and
  the group shows when it changed. *Reworded 2026-11-14: the earlier text said built-in entries
  only allow.*
- **R-37 Switches.** Every set has a switch for every workspace and an override per workspace, each
  on, off or unset; unset passes to the next level (the workspace's, then every workspace's, then the
  set's default: built-in sets off, the user's own sets on). A switch applies to the next request
  (R-8). Switching a set on closes the open requests it now decides, like a new rule (R-16), and
  says which. System managed has no switch.
- **R-38 Sets the user makes.** A set has a name (1 to 64 characters, unique among all sets,
  case-insensitive), a description, and entries that are ordinary rules with the scope `set`
  (exact or suffix, allow or deny, with an optional expiry, R-7). Entries are added like any rule,
  or by approving or denying a pending request **into the set** (R-15): the set must be on for
  the request's workspace, or the decision is refused (it would not allow the request). Deleting a
  set deletes its entries (`rule_deleted`, reason `set_deleted`) and its switches.
- **R-39 Precedence of sets and own rules.** Replaced by R-59 and R-60: an own rule, an entry of a
  group the user made and a built-in entry are ranked by one explicit precedence, not by the kind
  of list they sit in. Until precedence is built, the earlier ranking stands: own rules are
  ranked as R-6, set entries by pattern specificity then deny over allow, and an own rule decides
  unless a set's deny is strictly more specific than it. The examples of R-60 hold in both.
- **R-40 System managed is shown, with reasons.** The Rules screen lists every System managed
  host with its reason in words and where it applies (every workspace or one). Nothing in it is
  stored as a rule or hidden. To block one of its hosts, the user adds an own deny (R-60); to
  remove the reason, the user changes the choice behind it.
- **R-41 System managed follows the setup.** Puddle derives it from the settings at start and
  after every settings or consent change, and stores only the reasons (so a restart with the same
  setup changes and records nothing):

  | Setting | Hosts | Applies to |
  |---|---|---|
  | The browser editor runs Microsoft's VS Code server (consent granted for the terms Puddle shows) | `update.code.visualstudio.com`, `vscode.download.prss.microsoft.com`, `marketplace.visualstudio.com`, `*.gallery.vsassets.io`, `*.gallerycdn.vsassets.io` | every workspace |
  | The browser editor runs the bundled code-server (the default) | `open-vsx.org`, `openvsx.eclipsecontent.org` | every workspace |
  | Direct SSH is on for a workspace, for desktop VS Code | the five Microsoft hosts | that workspace |
  | Direct SSH is on for a workspace, for Cursor | `cursor.blob.core.windows.net` (server download), `marketplace.cursorapi.com`, `marketplace.cursor-cdn.com` (extensions) | that workspace |
  | Direct SSH is on for a workspace, for JetBrains Gateway | `download.jetbrains.com`, `download-cdn.jetbrains.com`, `download-cf.jetbrains.com`, `cache-redirector.jetbrains.com`, `data.services.jetbrains.com` (IDE backend), `plugins.jetbrains.com`, `downloads.marketplace.jetbrains.com` (plugins) | that workspace |
  | Direct SSH is on for a workspace, for Zed | `github.com`, `release-assets.githubusercontent.com` (remote server download) | that workspace |

  Open VSX is in force from the first start, on a fresh install with no settings stored: the
  default editor is code-server, so every workspace, including one whose browser editor was never
  opened, can install an extension from the editor's extension view with no rule added and no
  request raised. The registry's API (`open-vsx.org`) and its file host (`openvsx.eclipsecontent.org`,
  where a download is redirected) are both listed, because an install needs both. A user who
  does not want it adds an own deny (R-60).

  Telemetry, experiment and certificate-status hosts are not in it: they are ordinary traffic
  (R-1). When the reasons change, one `system_managed_changed` record per scope says which reasons
  were added and removed, and open requests the new hosts decide are closed by `system` (R-37).
  Direct SSH is the workspace's effective `direct_ssh` setting (its own value, else the global
  default).
  Direct SSH has no per-editor choice: switching it on gives the workspace every desktop editor's
  list above, each shown on the Rules screen with its own reason, and an own deny of any host
  overrides it (R-60). The AI, sign-in, licence and telemetry hosts of these editors are not in
  the lists.
  Zed's list holds `github.com` as a whole, because Zed's release download starts there and Puddle
  cannot tell that fetch from another one: with direct SSH on, the workspace can reach any GitHub
  repository, a wider allow than the other lists.
- **R-42 A set's allow reaches a local destination only like a wildcard.** After resolving a name
  a set allowed, an address in a local category counts as reached by a wildcard rule (R-14): with
  "wildcards reach local addresses" off, it needs an own exact allow of the name or the address,
  and goes pending for the exact name otherwise. A set's deny of an IP literal excludes that
  address like an own one (R-27).
- **R-43 Rule set records.** `rule_set_created`, `rule_set_updated` (rename, description or colour), `rule_set_deleted`
  (each with the set and the actor), `rule_set_switched` (`set_id`, `workspace_id` or `null` for
  every workspace, `enabled` true, false or `null`, actor), `rule_set_changed` (R-36) and
  `system_managed_changed` (R-41). A connection decided by a set's entry has `reason: rule`, the
  entry's `rule_id` (`null` for built-in and System managed entries) and `rule_set`; a pending
  row closed by one records the set in `rule_set`. A refusal by a set's deny carries
  `x-puddle-rule-set` next to `x-puddle-rule`.

- **R-57 Every rule set has a colour.** A set is shown with a *fill*, a tagged value
  (`{"kind": "solid", "color": "#rrggbb"}`) that is one solid colour today, so a gradient can be
  added later as another kind without a migration and a stored `solid` fill keeps reading. A
  built-in set's fill is part of its catalogue entry and fixed. A set the user made stores its
  own (a nullable column: a set with none, including every set made before colours, has the default
  of its number, taken round and round from a fixed palette). The API sends the fill on every set; on
  create it is optional, and on update leaving it out keeps the colour. Only a solid `#rrggbb` is
  accepted (either case, stored in lower case); anything else is a `422`. `rule_set_created`,
  `rule_set_updated` and `rule_set_deleted` carry the fill the user chose, and leave it out when
  there was none. A colour never decides anything: it is shown beside the set's name, never in
  its place, the ink on a filled chip is chosen for at least 4.5:1 contrast, and the edge of a
  dot or chip is the theme's border colour, so it shows in the light and the dark theme.

### Groups and precedence

*Specified 2026-11-14; built in slices. R-59 to R-66 replace R-39's ranking when built.*

The Rules screen shows rules in **groups** (what the store calls a rule set, R-38): the locked
*Built in* group with its parts, System managed under it, the groups the user made, and the rules
in no group. A group organises rules and switches them together. It never decides an order: what
decides is each rule's own precedence.

- **R-59 Every rule has a precedence.** An integer from 1 to 999, higher wins, default 500. It is
  stored on every rule the user can make: a host rule (global or workspace, in a group or in none),
  a rule on method and path (§8). Built-in entries and System managed hosts sit at the fixed
  precedence 0, below every rule the user can make; the user cannot change it. A rule keeps its
  precedence when it moves between groups. A group may hold a default precedence (1 to 999, none
  means 500) that a new rule in it takes when the request to add it names none; changing the default
  changes no existing rule. A migration gives every rule that exists before precedence the value 500.
- **R-60 Decision order.** Of the rules that match a request, the highest precedence decides. A rule
  matches only when it is switched on (R-65), unexpired (R-7), in scope for the workspace (R-5) and,
  for an entry of a group, the group is on for that workspace (R-37). Rules of the same precedence
  are ranked as R-6: (1) pattern specificity, (2) a `workspace` rule over a `global` one (an entry
  of a group counts as `global`), (3) `deny` over `allow`. Precedence comes first, so an own exact
  allow at 500 loses to an own `*.example.com` deny at 600, and an own allow of any pattern at 1
  beats a built-in block at 0. A group's own switches and a request's workspace never change a
  rule's precedence. Examples: an own deny of `*.visualstudio.com` beats System managed's
  `update.code.visualstudio.com` (500 over 0); an own deny of `.example.com` and an own exact allow
  of `a.example.com` at the same precedence: the allow wins by specificity (R-6), and R-63 warns;
  the same two with the deny at 600: the deny wins. A rule on method and path (§8) is checked after
  the host's decision, as R-50 says, and refuses whatever the precedence of the host rule that
  allowed the host: a rule that only refuses cannot open anything. Its precedence is stored, shown
  and compared only among rules of its own kind, where it has no effect today because they only refuse.
- **R-61 What the migration changes.** Own rules rank among themselves as before (R-6 is the tie
  rule), and built-in and System managed entries keep their place below every own rule: no answer
  changes for them. A built-in entry that blocks and an own allow of the same host used to be
  decided by pattern specificity; no built-in block has shipped, so no answer changes. The entries of
  groups the user made move from "below own rules, except a strictly more specific deny" to the same
  precedence as own rules, so an answer changes only where such an entry and an own rule overlap
  with opposite effects and one of two cases holds: (A) the group's allow is more specific than
  an own deny (today the own deny wins, now the allow wins), or (B) the group's deny and a global
  own allow are equally specific (today the allow wins, now the deny wins). The migration finds
  every such pair, writes one `precedence_migrated` record that names them with the old and new
  answer, and shows it once on the Rules screen; setting a precedence restores any answer. The
  screen and the API say "group" where the store and the routes say "set".
- **R-62 A request names the rules that matched.** The `connection` record of a request a rule
  decided (§5; R-25 holds: no query, no header value) carries `precedence` (the winning rule's) and `matches`, a list of every rule that
  matched it, at most 20 (the winner first, then by precedence, then as R-60), with a count of the
  rest in `more`. Each item is `{ kind, id, effect, precedence, won }`: `kind` is `rule`, `request_rule`
  or `builtin`; `id` is the rule's id, the request rule's id, or for a built-in entry its group and
  pattern; `won` marks the rule that decided. The record also carries a `link` per item, the Rules
  screen address that opens and highlights that row (`/rules?rule=<id>`, `/rules?request-rule=<id>`,
  `/rules?builtin=<group>&pattern=<pattern>`); a deleted rule is plain text. A refusal's `403` states
  the winning rule's precedence in words and in `x-puddle-precedence`; it still does not repeat the
  path or the query (R-53). A request no rule matched is pending and has no matches. The Activity
  screen shows the matches and their links.
- **R-63 Overlap warning.** Two rules **overlap** when both match some host, they can apply to the
  same workspace (two `workspace` rules of different workspaces never do), both are switched on and
  unexpired, and their precedence is equal. An overlap is a warning, not a refusal, when the rules
  have opposite effects or are duplicates (same pattern, scope and effect); two rules of the
  same effect where one pattern contains the other overlap harmlessly and warn of nothing. The
  warning says in words which rule decides and why: "decided by specificity: `a.example.com` beats
  `*.example.com`", "decided by workspace scope", or "deny wins at equal rank", and says that a
  different precedence makes the order explicit. Giving one of the two another precedence, or
  switching one off, clears it. Rules on method and path warn only for a duplicate (same host
  pattern, methods and path pattern); overlap of different path patterns is not judged. The API
  returns the overlaps of each rule, computed from the engine's index, and an overlap is never an
  audit record.
- **R-64 Conflict error.** A rule **conflicts** with another when they have the same pattern and the
  same scope, opposite effects, equal precedence, and both are switched on and unexpired: the only
  thing that would decide is "deny wins". That is an error, shown in red on both rows. Saving
  a rule that would create a conflict (adding, editing, importing, moving, or switching
  it on, R-65) is refused with `409` and the code `rule_conflict`, naming the other rule: the
  rule that creates it is not saved, and nothing else is blocked. Switching a group or a built-in
  entry on is never refused, because a switch is the user's safety control; a conflict that arises
  that way, and one that already exists after the migration, stays shown as an error, and deny
  wins meanwhile (R-6). A row of an import that would conflict is skipped, counted and listed in
  the preview and the result, and the other rows are imported. The fix is another precedence, another
  scope, or removing a rule. Built-in entries never conflict with the user's rules (precedence 0
  against 1 to 999); two built-in entries that would conflict fail the build (R-36).
- **R-65 A rule can be switched off.** Every rule and rule on method and path has an `enabled`
  flag (default on). A rule that is off never matches, appears in no `matches` list, takes part
  in no overlap or conflict, and keeps its precedence and place. A single entry of a built-in
  group has a switch of its own, stored apart from the catalogue (the catalogue is never copied into
  the database, R-36). Switching a rule on or off applies to the next request (R-8) and writes
  `rule_switched` (the rule, the new state, the actor). Switching one on closes the open requests
  it now decides, like a new rule (R-16).
- **R-66 What is recorded.** `rule_switched` (R-65), `rule_updated` (a rule edited in place:
  the fields that changed, including precedence, with the actor), `precedence_migrated` (R-61) and
  `rules_imported` (the group, the counts, the skipped conflicts). They use the actors of §1.

## 8. Rules on method and path

A rule on a host decides whether a workspace may reach it. A rule on method and path narrows that
for a host the workspace reaches anyway: "this host, but never `DELETE` on `/repos/**`". It only
ever refuses, so no rule of this kind can open anything: it is checked after the host's decision and
refuses whatever precedence decided it (R-60). It carries a precedence (R-59) so that it sorts and
warns with the other rules, and so that a rule on method and path that allows can be ranked when
one exists.

- **R-49 A rule on method and path** names a host (`example.com` or `*.example.com`, the grammar of
  the never-decrypt names), the methods it covers (none named: every method) and a path pattern
  (R-51). It applies to every workspace or to one. It is made and deleted over the API and has
  its own numbering, apart from the rules of R-1: `rule_id` never names one. A scope holds at
  most 500. A change reaches every running workspace at once (the `request_rules_changed` event),
  with no restart, and applies from the next connection: a connection already open keeps judging
  its requests by the rules it began with. The records are `request_rule_created` and
  `request_rule_deleted`
  (R-24); deleting a workspace deletes its rules, one record each, and keeps the global ones.
- **R-50 Where it is checked.** On every request Puddle can read: each request of a decrypted
  HTTP/1.1 connection (a keep-alive connection is judged request by request) and each stream of
  an HTTP/2 connection, and plain-HTTP requests. A host with a rule is decrypted whatever the
  decrypt-every-host switch, the never-decrypt names and the learned list say, and for a client
  the guest agent marks as being in a container too, because a request that cannot be read
  cannot be judged. A tunnel to another port of such a host is refused with
  the reason `request_rule_port`, since its requests would never be seen. The check comes before
  anything is sent to the server and before a credential is added. Because the rule overrides
  the never-decrypt names, a rule whose host one of them names (the global ones, and the
  workspace's own or, for a global rule, any workspace's) carries a `warning` in the API that
  names them: a tool there that does not trust Puddle's certificate fails on that host. A connection Puddle passes
  through without decrypting it (a never-decrypt name, a learned host, any host without a rule
  or credential while decrypt-every-host is off) is not read, so a request inside it is never
  judged, even one for a host with a rule: an HTTP/2 client that reuses such a connection for
  another name the same server answers, or a guest that names a host with a rule inside a tunnel
  to another host (domain fronting), reaches that host unchecked. Puddle gives every name its own
  stand-in address, so a client does not share a connection between two names on its
  own; one that is told to (an HTTP/2 `ORIGIN` frame) or a hostile guest can. A rule on method and
  path is a guard against an agent's mistakes, not against a guest that sets out to evade it
  (R-52).
- **R-51 A path pattern** starts with `/` and is made of segments: a name, `*` (exactly one
  segment) or, last, `**` (any number of further segments, none included). `/gists` is that path
  alone, `/gists/**` is `/gists` and everything below it, `/**` is every path. `*` or `**` as part
  of a segment (`/a*`), a `**` that is not last, a query string or fragment, more than 32
  segments and more than 512 bytes are refused when the rule is made. The path of a request is
  compared in a canonical form that errs towards matching more, because a rule only refuses: the
  query is cut and never read; `%XX` and `%uXXXX` are decoded up to four times; `\` counts as `/`;
  empty and `.` segments go and `..` removes the segment before it; `;parameters` of a segment
  go; trailing dots and spaces of a segment go; a NUL ends the path; case is ignored; a `#` is judged
  both as the start of a fragment and as a plain character of the path (a server that keeps it
  reads `/public#/../admin` as `/admin`). A pattern is
  written in the same form, so `/Gists/` and `/%67ists` are one pattern. Two paths that differ only
  in such spellings are one path here even where a server would tell them apart: that costs a
  request a refusal, never a bypass. Where servers disagree, the path is judged in every reading
  one of them may make, and a rule that names any reading refuses the request: decoded before or
  after it is split at `/` (a router that matches undecoded segments reads `/repos/a%2fb` as
  `repos` and `a/b`, so `/repos/*` names it); `;parameters` kept or dropped; `..` counting an
  empty segment or not; `\` a separator or part of a name; a NUL ending the path or kept; an
  escaped `.` or `..` resolved or kept as a name; one round of decoding or four; `.` and `..`
  with trailing spaces or control characters recognised or not; trailing dots alone or trailing
  dots and spaces in any mix dropped from a segment; a `?` or `#` that only decoding
  reveals ending the path; and a leading `//name` read as an authority (`//evil.example/admin` is
  `/admin` to a server that resolves the target against a base URL).
- **R-52 A method** is matched ignoring case. A request is also judged as every method it asks
  to be read as: an `X-HTTP-Method-Override`, `X-HTTP-Method` or `X-Method-Override` header and a
  `_method` key in the query string, read as PHP reads it (pairs split at `&` or `;`, `+` a space,
  `.` and a space in a key read as `_`; a `+` in the value read both as a space and as written,
  since it is a token character). A rule on `DELETE` therefore refuses a `POST` that asks to
  be a `DELETE`. An HTTP/2 extended `CONNECT` (a WebSocket over HTTP/2) is also judged as `GET`,
  the method an HTTP/1.1 server is sent for it, so a rule written for a WebSocket upgrade names
  it. Methods are HTTP tokens, at most 16 per rule. A rule cannot see a method carried in
  the request body (a `_method` form field that some frameworks honour) and does not decode spellings
  only a server's own decoder understands (overlong UTF-8, look-alike Unicode): both are limits of
  a rule on method and path, so a rule is a guard against an agent's ordinary mistakes and its
  common tricks, not a proof against a guest that knows the server.
- **R-53 The refusal** is a `403` whose body says in words that a rule refuses requests like this
  one and names the rule, with `x-puddle-decision: deny` and `x-puddle-request-rule: <id>`. It
  does not repeat the path or the query, so nothing of them lands in a line the tool prints.
- **R-54 Audit.** A refused request is the connection's `connection` record with `decision: deny`,
  `reason: request_rule`, `request_rule` (the rule's id, `null` in every other record), and the
  `method` and `path` of the refused request (R-25 holds: no query, no header value). A decrypted
  connection that carries several requests has one record: the first refused request is the one it names,
  and a connection no rule refused names its first request as before. A refused tunnel is
  `decision: blocked`, `reason: request_rule_port`, with `request_rule` the lowest-numbered rule
  on the host (its `403` carries the same id in `x-puddle-request-rule`).
- **R-55 No rule of this kind is ever made by Puddle.** Automated help may only allow (principle 2; a built-in group may block hosts, R-36, but never by method and path), so
  rule sets, System managed and the AI judge never create one; every rule on method and path
  was made by the user over the API (a screen for them is not built yet) and shows in the audit with who made it.

## 9. Out of scope here

The AI judge (no field or placeholder until its flow is designed), subscribed lists (a rule set
fetched from a URL), rules on method and path that allow, or that match on headers or a body,
port rules, and the "Dangerous settings" unblock of
Puddle's endpoints (later). Address classification, name normalisation and the 403 body belong to
the proxy and `puddle-netpolicy`; the API and CLI shape of R-15 to `puddle-api` and `puddle`.
