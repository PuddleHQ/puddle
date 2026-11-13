// SPDX-License-Identifier: AGPL-3.0-or-later
// What the activity screen shows of an audit record, and how its filters turn into an API query
// and a page address. Pure: nothing here touches the page or the network.
import type { components } from "#lib/api/schema.d.ts";
import { actorLabel } from "#lib/rules/model.ts";

type Schemas = components["schemas"];
export type AuditEntry = Schemas["AuditEntry"];
export type AuditRecord = Schemas["AuditRecord"];
export type AuditType = Schemas["AuditType"];
export type AuditOutcome = Schemas["AuditOutcome"];

/** The newest matching records come first and the log is read back from there. */
export const PAGE_SIZE = 200;
/** The API's cap per page, used when the whole filtered log is exported. */
export const EXPORT_PAGE_SIZE = 500;

export type RangeKey = "1h" | "24h" | "7d" | "all";

export const RANGES: readonly { key: RangeKey; label: string; ms: number }[] = [
  { key: "1h", label: "Last hour", ms: 3_600_000 },
  { key: "24h", label: "Last 24 hours", ms: 86_400_000 },
  { key: "7d", label: "Last 7 days", ms: 7 * 86_400_000 },
  { key: "all", label: "Everything", ms: 0 },
];

export const DEFAULT_RANGE: RangeKey = "24h";

export type AuditCategory = "company_network";

export const CATEGORIES: readonly { value: AuditCategory; label: string }[] = [
  { value: "company_network", label: "Company network" },
];

/** What put a destination on the company network, as a record's `signal` code reads in words. */
const COMPANY_SIGNAL_WORDS: Record<string, string> = {
  pac_direct: "The company's proxy script sends it straight out.",
  dns_suffix: "Its name ends in one of the company's DNS suffixes.",
  private_access_cgnat: "A company private-access client's range.",
  vpn_route: "Reached through a VPN connection.",
};

export interface Filter {
  /** A workspace's id; empty for all. */
  workspace: string;
  type: AuditType | "";
  outcome: AuditOutcome | "";
  /** A destination category; empty for all. Only the company network exists. */
  category: AuditCategory | "";
  /** Text a record's host (or a rule's pattern), method, path or reason contains; empty for all. */
  q: string;
  range: RangeKey;
}

export const NO_FILTER: Filter = {
  workspace: "",
  type: "",
  outcome: "",
  category: "",
  q: "",
  range: DEFAULT_RANGE,
};

export const TYPES: readonly { value: AuditType; label: string }[] = [
  { value: "connection", label: "Connection" },
  { value: "pending_created", label: "Request opened" },
  { value: "pending_decided", label: "Request decided" },
  { value: "pending_expired", label: "Request closed" },
  { value: "pending_suppressed", label: "Requests refused for volume" },
  { value: "rule_created", label: "Rule added" },
  { value: "rule_updated", label: "Rule changed" },
  { value: "rule_deleted", label: "Rule deleted" },
  { value: "rule_expired", label: "Rule expired" },
  { value: "request_rule_created", label: "Method and path rule added" },
  { value: "request_rule_deleted", label: "Method and path rule deleted" },
  { value: "rule_set_created", label: "Rule set added" },
  { value: "rule_set_updated", label: "Rule set renamed" },
  { value: "rule_set_deleted", label: "Rule set deleted" },
  { value: "rule_set_switched", label: "Rule set switched" },
  { value: "rule_set_changed", label: "Rule set updated by Puddle" },
  { value: "system_managed_changed", label: "System managed changed" },
  { value: "workspace_op", label: "Workspace operation" },
  { value: "audit_trimmed", label: "Log trimmed" },
];

export const OUTCOMES: readonly { value: AuditOutcome; label: string }[] = [
  { value: "allow", label: "Allowed" },
  { value: "deny", label: "Denied" },
  { value: "pending", label: "Waiting" },
  { value: "blocked", label: "Blocked" },
  { value: "expired", label: "Closed" },
  { value: "succeeded", label: "Succeeded" },
  { value: "failed", label: "Failed" },
];

const typeLabels = new Map(TYPES.map((t) => [t.value, t.label]));
const outcomeLabels = new Map(OUTCOMES.map((o) => [o.value, o.label]));

export function typeLabel(type: AuditType): string {
  return typeLabels.get(type) ?? type;
}

export function outcomeLabel(outcome: AuditOutcome): string {
  return outcomeLabels.get(outcome) ?? outcome;
}

/** True when any filter differs from the screen's defaults (the "Clear filters" button shows). */
export function isFiltered(filter: Filter): boolean {
  return (
    filter.workspace !== "" ||
    filter.type !== "" ||
    filter.outcome !== "" ||
    filter.category !== "" ||
    filter.q.trim() !== "" ||
    filter.range !== DEFAULT_RANGE
  );
}

/** True when the filter leaves out part of the log (so "no records" may just mean "none match"). */
export function isNarrowed(filter: Filter): boolean {
  return (
    filter.workspace !== "" ||
    filter.type !== "" ||
    filter.outcome !== "" ||
    filter.category !== "" ||
    filter.q.trim() !== "" ||
    filter.range !== "all"
  );
}

/** The server-side filters of `GET /api/audit`; `now` fixes a relative range. */
export interface AuditQuery {
  workspace?: string;
  type?: AuditType;
  outcome?: AuditOutcome;
  category?: AuditCategory;
  q?: string;
  from?: number;
}

export function toQuery(filter: Filter, now: number): AuditQuery {
  const query: AuditQuery = {};
  if (filter.workspace !== "") query.workspace = filter.workspace;
  if (filter.type !== "") query.type = filter.type;
  if (filter.outcome !== "") query.outcome = filter.outcome;
  if (filter.category !== "") query.category = filter.category;
  const q = filter.q.trim();
  if (q !== "") query.q = q;
  const range = RANGES.find((r) => r.key === filter.range);
  if (range && range.ms > 0) query.from = now - range.ms;
  return query;
}

/** The page address's query string for a filter (`""` when it is the default view). */
export function toSearch(filter: Filter): string {
  const params = new URLSearchParams();
  if (filter.workspace !== "") params.set("workspace", filter.workspace);
  if (filter.type !== "") params.set("type", filter.type);
  if (filter.outcome !== "") params.set("outcome", filter.outcome);
  if (filter.category !== "") params.set("category", filter.category);
  if (filter.q.trim() !== "") params.set("q", filter.q.trim());
  if (filter.range !== DEFAULT_RANGE) params.set("range", filter.range);
  const text = params.toString();
  return text === "" ? "" : `?${text}`;
}

/** A filter from a page address; anything it does not recognise is left at the default. */
export function fromSearch(search: string): Filter {
  const params = new URLSearchParams(search);
  const type = params.get("type");
  const outcome = params.get("outcome");
  const range = params.get("range");
  const category = params.get("category");
  return {
    workspace: params.get("workspace") ?? "",
    type: TYPES.some((t) => t.value === type) ? (type as AuditType) : "",
    outcome: OUTCOMES.some((o) => o.value === outcome)
      ? (outcome as AuditOutcome)
      : "",
    category: CATEGORIES.some((c) => c.value === category)
      ? (category as AuditCategory)
      : "",
    q: params.get("q") ?? params.get("host") ?? "",
    range: RANGES.some((r) => r.key === range)
      ? (range as RangeKey)
      : DEFAULT_RANGE,
  };
}

export type Tone = "allow" | "deny" | "warn" | "neutral";

/** One record as a table row reads it. Every string can quote a guest and is shown as text only. */
export interface RowView {
  /** Epoch ms. */
  ts: number;
  /** The workspace; `null` for what is not about one (a global rule, a trim). */
  workspace: string | null;
  type: string;
  /** The host (and port), or a rule's pattern; `null` when there is none. */
  destination: string | null;
  outcome: { label: string; tone: Tone } | null;
  /** What else is worth a glance: why, how much, which rule. */
  detail: string;
  /** The rule on method and path that refused a connection; its name is part of `detail`. */
  requestRule?: number;
  /** Set on a connection to a company-network destination: the chip's text and what it means. */
  company?: { label: string; title: string };
}

/** How a rule on method and path reads inside a connection's detail. */
export function requestRuleName(id: number): string {
  return `method and path rule ${id}`;
}

/** A detail line cut around the name of its rule, so the name can be a link. */
export function splitDetail(
  detail: string,
  id: number,
): { before: string; name: string; after: string } {
  const name = requestRuleName(id);
  const at = detail.indexOf(name);
  if (at < 0) return { before: detail, name: "", after: "" };
  return {
    before: detail.slice(0, at),
    name,
    after: detail.slice(at + name.length),
  };
}

const REASONS: Record<string, string> = {
  rule: "a rule",
  no_rule: "no rule yet",
  suppressed: "more records came in one second than Puddle writes",
  merged:
    "Puddle merged the oldest records written while requests waited, to keep the log small (their hosts are not kept)",
  request_rule: "a rule on method and path",
  request_rule_port:
    "the host has rules on method and path, which Puddle can only apply to HTTPS on port 443 and plain HTTP",
  puddle_endpoint: "Puddle's own address",
  ssh_unsupported: "SSH is not supported",
  udp_unsupported: "UDP is not supported",
  local_address: "a local address",
  "toggle:loopback": "this computer's own addresses are switched off",
  "toggle:private": "private networks are switched off",
  "toggle:link_local": "link-local addresses are switched off",
  "toggle:metadata": "cloud metadata addresses are switched off",
  "toggle:special": "other special ranges are switched off",
  "toggle:company_network": "company network destinations are switched off",
  settings_unreadable: "unreadable settings",
  puddle_request: "Puddle's own request",
  pki_distribution_point:
    "a certificate check address named in your company's root certificates",
  push_denied:
    'the push is not on the workspace\'s push list yet (click "Allow push" in the notice, or turn off "Only push to listed repos" on its Git tab)',
  pull_denied:
    'the fetch is not on the workspace\'s pull list yet (click "Allow pull" in the notice, or turn off "Only pull from listed repos" on its Git tab)',
  bad_git_path:
    "the Git address is not one Puddle reads, or names the repository in more than one way (use owner/repository)",
  ssh_command_unsupported:
    "Puddle serves only git-upload-pack and git-receive-pack over SSH, and this command was neither",
  ssh_channel_refused:
    "the SSH session asked for something Puddle does not allow (a terminal, a subsystem, forwarding or a second command)",
  credential_rejected:
    "the Git host did not accept the credential Puddle added",
  credential_unavailable: "the Git credential could not be read",
  credential_ambiguous: "two identities cover the repository",
  git_settings_unavailable: "the workspace's Git settings could not be read",
  body_too_large: "the request body was too large for Puddle to read",
  answer_encoding:
    "the answer came in an encoding Puddle cannot read, so it could not be checked for real secret values",
  answer_withheld:
    "the answer held a real secret value in a form Puddle cannot replace, so it was cut off",
  bad_request: "the request was malformed",
  misdirected_host: "the request was for another host than its connection",
  expectation_unsupported: "the request asked for more than Puddle supports",
  head_too_large: "the request head was too large",
  request_timeout: "the request did not arrive in time",
  protocol_unsupported: "the request used a protocol Puddle does not support",
};

/** Why one of Puddle's own requests (listing repositories) did not work: the code after
 * `puddle_request_failed:`. */
const FAILED_REQUEST: Record<string, string> = {
  unreachable: "the host could not be reached",
  tls: "its certificate was not trusted",
  timeout: "it did not answer in time",
  too_large: "the answer was too large",
  protocol: "it did not answer like a Git host",
  bad_token: "the token cannot be sent",
  host_refused: "the Git host refused it (check the sign-in)",
  rate_limited: "the Git host's request limit is used up (try again later)",
  host_error: "the Git host answered with an error",
};
const FAILED_PREFIX = "puddle_request_failed:";

/** What an allowed connection came to when reaching the destination failed. A connection
 * that was made (`connected`) or never tried (`null`) has no note. */
const OUTCOME_NOTES: Record<string, string> = {
  upstream_refused: "the company proxy refused it",
  connect_failed: "Puddle could not connect to it",
  timeout: "it did not answer in time",
};

/** Reasons that say something happened to an allowed connection, shown beside the rule. */
const NOTES: Record<string, string> = {
  guest_tls_rejected:
    "the tool in the workspace didn't accept Puddle's certificate (it may keep its own list of trusted roots)",
  certificate_unavailable:
    "Puddle could not make its certificate for this host, so the connection failed on Puddle's side",
};

const DECISIONS: Record<string, { label: string; tone: Tone }> = {
  allow: { label: "Allowed", tone: "allow" },
  deny: { label: "Denied", tone: "deny" },
  pending: { label: "Waiting", tone: "warn" },
  blocked: { label: "Blocked", tone: "deny" },
};

/** A decision this build has no word for: shown as it came, never as "no outcome". */
function decisionOutcome(decision: string): { label: string; tone: Tone } {
  return DECISIONS[decision] ?? { label: decision, tone: "neutral" };
}

/** `1.5 MB`, in powers of 1024 as file sizes read. */
export function bytesLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const text = value >= 10 ? value.toFixed(0) : value.toFixed(1);
  return `${text} ${units[unit]}`;
}

function endpoint(host: string | null, port: number | null): string | null {
  if (host === null) return null;
  return port === null ? host : `${host}:${port}`;
}

function requestRuleLabel(rule: Schemas["AuditRequestRule"]): string {
  const where =
    rule.scope === "global"
      ? "every workspace"
      : `workspace ${rule.workspace_id ?? "?"}`;
  const methods =
    rule.methods.length === 0 ? "any method" : rule.methods.join(", ");
  return `refuse ${methods} ${rule.path} for ${where}`;
}

function ruleLabel(rule: Schemas["AuditRule"]): string {
  if (rule.scope === "set")
    return `${rule.effect} in rule set ${rule.set_id ?? "?"}`;
  const where =
    rule.scope === "global"
      ? "every workspace"
      : `workspace ${rule.workspace_id ?? "?"}`;
  return `${rule.effect} for ${where}`;
}

const SYSTEM_REASONS: Record<string, string> = {
  microsoft_server: "Microsoft's VS Code server",
  code_server: "code-server",
  direct_ssh: "direct SSH",
};

/** How each operation reads when it went through and when it failed. */
const WORKSPACE_OPS: Record<
  Schemas["WorkspaceOpKind"],
  { done: string; failed: string }
> = {
  create: { done: "Created", failed: "Create failed" },
  delete: { done: "Deleted", failed: "Delete failed" },
  rename: { done: "Renamed", failed: "Rename failed" },
  start: { done: "Started", failed: "Start failed" },
  stop: { done: "Stopped", failed: "Stop failed" },
  identity_created: {
    done: "Identity added",
    failed: "Adding an identity failed",
  },
  identity_updated: {
    done: "Identity changed",
    failed: "Changing an identity failed",
  },
  identity_deleted: {
    done: "Identity deleted",
    failed: "Deleting an identity failed",
  },
  identity_default_set: {
    done: "Default identity changed",
    failed: "Changing the default identity failed",
  },
  identities_changed: {
    done: "Identities changed",
    failed: "Changing identities failed",
  },
  identities_reordered: {
    done: "Identities reordered",
    failed: "Reordering identities failed",
  },
  repo_added: {
    done: "Repository added",
    failed: "Adding a repository failed",
  },
  repo_switched: {
    done: "Repository switches changed",
    failed: "Changing repository switches failed",
  },
  repo_removed: {
    done: "Repository removed",
    failed: "Removing a repository failed",
  },
  root_disk_grown: {
    done: "Root disk made bigger",
    failed: "Making the root disk bigger failed",
  },
  git_repaired: {
    done: "Git repaired after a crash",
    failed: "Repairing git after a crash failed",
  },
};

/** What was done and how it ended, then the plain sentence the record carries and who asked. */
function workspaceOpDetail(
  record: Extract<AuditRecord, { type: "workspace_op" }>,
): string {
  const words = WORKSPACE_OPS[record.op];
  const label =
    words === undefined
      ? record.op
      : record.outcome === "failed"
        ? words.failed
        : words.done;
  const note = record.detail === null ? "" : `: ${record.detail}`;
  return `${label}${note}, by ${record.actor}`;
}

function reasonList(reasons: readonly string[]): string {
  return reasons.map((r) => SYSTEM_REASONS[r] ?? r).join(", ");
}

/** "System managed", "rule set builtin:github", or `null` when no set decided (also for records
 * written before rule sets existed, which have no such field). */
function setLabel(set: string | null | undefined): string | null {
  if (set === undefined || set === null) return null;
  return set === "system" ? "System managed" : `rule set ${set}`;
}

function switchLabel(enabled: boolean | null): string {
  if (enabled === null) return "follows the next level";
  return enabled ? "on" : "off";
}

/** Why a request was closed without an answer. Puddle closes one only with its workspace; the
 * seven-day reason is in records an earlier version wrote. */
function closedDetail(reason: string): string {
  if (reason === "workspace_deleted")
    return "closed: its workspace was deleted";
  if (reason === "stale")
    return "closed: not repeated for 7 days (Puddle no longer does this)";
  return `closed: ${reason}`;
}

function pendingOutcome(state: string): RowView["outcome"] {
  if (state === "allowed") return decisionOutcome("allow");
  if (state === "denied") return decisionOutcome("deny");
  if (state === "expired") return { label: "Closed", tone: "neutral" };
  if (state === "requested" || state === "pending") {
    return decisionOutcome("pending");
  }
  return { label: state, tone: "neutral" };
}

/**
 * What a record's method field holds when the connection was Git over SSH: the proxy writes the
 * command (`git-receive-pack`) there, where HTTP writes `POST`. A command Puddle did not serve
 * (`ssh-command`) and a refused channel request (`ssh-channel`) are written the same way. The
 * record has no protocol field yet, and no HTTP method is spelled in lower case with a hyphen.
 */
const SSH_COMMANDS: Record<string, string> = {
  "git-receive-pack": "push to",
  "git-upload-pack": "fetch from",
  "git-lfs-upload": "Git LFS upload to",
  "git-lfs-download": "Git LFS download from",
};
const SSH_REFUSED_WORDS: Record<string, string> = {
  "ssh-command": "command Puddle does not serve",
  "ssh-channel": "session request",
};

/** "SSH: push to github.com/acme/app", or `null` when the record is not Git over SSH. */
function sshLine(
  record: Extract<AuditRecord, { type: "connection" }>,
  method: string,
): string | null {
  const verb = SSH_COMMANDS[method];
  if (verb !== undefined) {
    return `SSH: ${verb} ${record.host ?? ""}${record.path ?? ""}`;
  }
  const refused = SSH_REFUSED_WORDS[method];
  return refused === undefined ? null : `SSH: ${refused}`;
}

function connectionDetail(
  record: Extract<AuditRecord, { type: "connection" }>,
): string {
  const parts: string[] = [];
  if (record.method !== null) {
    parts.push(
      sshLine(record, record.method) ??
        `${record.method} ${record.path ?? ""}`.trim(),
    );
  }
  const failed = record.reason.startsWith(FAILED_PREFIX)
    ? record.reason.slice(FAILED_PREFIX.length)
    : null;
  const reason =
    failed === null
      ? (REASONS[record.reason] ?? record.reason)
      : `Puddle's own request failed: ${FAILED_REQUEST[failed] ?? failed}`;
  const note = NOTES[record.reason];
  const set = setLabel(record.rule_set);
  if (set !== null) parts.push(set);
  if (record.request_rule !== null) {
    parts.push(requestRuleName(record.request_rule));
  } else if (record.rule_id !== null) parts.push(`rule ${record.rule_id}`);
  else if (set === null && note === undefined) parts.push(`because ${reason}`);
  if (note !== undefined) parts.push(note);
  if (record.count !== null) {
    parts.push(`${record.count} ${record.count === 1 ? "record" : "records"}`);
  }
  if (record.bytes_up > 0 || record.bytes_down > 0) {
    parts.push(
      `${bytesLabel(record.bytes_up)} up, ${bytesLabel(record.bytes_down)} down`,
    );
  }
  const unreached = unreachedNote(record);
  if (unreached !== null) parts.push(unreached);
  if (record.injected) parts.push("credential added");
  if (record.refused_address !== null) {
    const why = record.refused_reason ?? "";
    parts.push(
      why === "ip_rule"
        ? `the answer held ${record.refused_address}, which a rule on that address denies, so Puddle did not use it`
        : `the answer also held ${record.refused_address} (${REASONS[why] ?? why}), which Puddle did not use`,
    );
  }
  if (record.unswapped_stand_ins.length > 0) {
    parts.push(unswappedNote(record.unswapped_stand_ins));
  }
  if (record.foreign_credentials.length > 0) {
    parts.push(foreignNote(record.foreign_credentials));
  }
  if (record.answer_credentials.length > 0) {
    parts.push(unknownTokenNote(record.answer_credentials));
  }
  if (record.scrubbed_stand_ins.length > 0) {
    const names = record.scrubbed_stand_ins.map(standInLabel).join(", ");
    parts.push(
      `the answer held the real value of ${names}; the workspace got the stand-in back`,
    );
  }
  return parts.join(" · ");
}

const FOREIGN_KINDS: Record<string, string> = {
  stale_stand_in: "a stand-in Puddle no longer holds",
  github_token: "a GitHub token",
  npm_token: "an npm token",
  gitlab_token: "a GitLab token",
  azure_devops_token: "an Azure DevOps token",
  git_authorization: "a login for a Git repository",
};

/** The kinds of credential the workspace sent that Puddle did not add; never a value. */
function foreignNote(kinds: readonly string[]): string {
  const names = kinds.map((k) => FOREIGN_KINDS[k] ?? k).join(", ");
  return `the workspace sent ${names} that Puddle did not add; it went out as it was`;
}

const UNKNOWN_TOKEN_KINDS: Record<string, string> = {
  github_token: "a GitHub token",
  github_fine_grained_token: "a fine-grained GitHub token",
  gitlab_token: "a GitLab token",
  npm_token: "an npm token",
  azure_devops_token: "an Azure DevOps token",
  anthropic_key: "an Anthropic key",
  openai_key: "an OpenAI key",
  jwt_with_refresh_token: "a sign-in token with a refresh token",
};

/** The kinds of token the answers held that Puddle does not hold; never a value. */
function unknownTokenNote(kinds: readonly string[]): string {
  const names = kinds.map((k) => UNKNOWN_TOKEN_KINDS[k] ?? k).join(", ");
  return `the answer held ${names} that Puddle does not hold; the workspace got it as it was`;
}

/**
 * A stand-in id is `stand-in:<secret|login>:<name>`; a name carries no colon. A stale one,
 * shaped like a stand-in that this workspace does not hold, is `stale-stand-in:secret:<name>` or
 * `stale-stand-in:github`.
 */
function standInLabel(id: string): string {
  const [prefix, kind, name] = id.split(":");
  if (prefix !== "stand-in" || name === undefined) return id;
  return kind === "login" ? `login ${name}` : name;
}

function staleLabel(id: string): string {
  const [, , name] = id.split(":");
  return name === undefined ? "a GitHub login" : name;
}

function unswappedNote(ids: readonly string[]): string {
  const stale = ids.filter((id) => id.startsWith("stale-stand-in:"));
  const unswapped = ids.filter((id) => !id.startsWith("stale-stand-in:"));
  const notes: string[] = [];
  if (unswapped.length > 0) {
    const names = unswapped.map(standInLabel).join(", ");
    notes.push(
      `stand-in for ${names} went out unchanged (check the secret's hosts, and that the tool sends it in a header, not in the URL)`,
    );
  }
  if (stale.length > 0) {
    const names = stale.map(staleLabel).join(", ");
    notes.push(
      `${names} looks like a stand-in this workspace no longer holds, so it went out unchanged (the secret was removed or the login forgotten: set it again or sign in again, and restart the tool that keeps the old value)`,
    );
  }
  return notes.join(" · ");
}

function unreachedNote(
  record: Extract<AuditRecord, { type: "connection" }>,
): string | null {
  return record.outcome === null
    ? null
    : (OUTCOME_NOTES[record.outcome] ?? null);
}

function connectionOutcome(
  record: Extract<AuditRecord, { type: "connection" }>,
): RowView["outcome"] {
  if (
    record.reason.startsWith(FAILED_PREFIX) ||
    unreachedNote(record) !== null
  ) {
    return { label: "Failed", tone: "warn" };
  }
  return record.decision === null ? null : decisionOutcome(record.decision);
}

/** How a record reads as a row. */
export function describe(record: AuditRecord): RowView {
  const type = typeLabel(record.type);
  switch (record.type) {
    case "connection":
      return {
        ts: record.ts,
        workspace: record.workspace_id,
        type,
        destination: endpoint(record.host, record.port),
        outcome: connectionOutcome(record),
        detail: connectionDetail(record),
        ...(record.category === "company_network"
          ? {
              company: {
                label: "Company network",
                title:
                  (record.signal && COMPANY_SIGNAL_WORDS[record.signal]) ||
                  "On the company network.",
              },
            }
          : {}),
        ...(record.request_rule === null
          ? {}
          : { requestRule: record.request_rule }),
      };
    case "pending_created":
    case "pending_decided":
    case "pending_expired": {
      const p = record.pending;
      const detail =
        record.type === "pending_expired"
          ? closedDetail(record.reason)
          : record.type === "pending_decided"
            ? `by ${p.decided_by ?? "unknown"}${p.rule_id === null ? "" : `, rule ${p.rule_id}`}${setLabel(p.rule_set) === null ? "" : `, ${setLabel(p.rule_set)}`}`
            : p.attempts > 1
              ? `${p.attempts} attempts`
              : "";
      return {
        ts: record.ts,
        workspace: p.workspace_id,
        type,
        destination: endpoint(p.host, p.port),
        outcome:
          record.type === "pending_created"
            ? pendingOutcome("requested")
            : record.type === "pending_expired"
              ? pendingOutcome("expired")
              : pendingOutcome(p.state),
        detail,
      };
    }
    case "pending_suppressed":
      return {
        ts: record.ts,
        workspace: record.workspace_id,
        type,
        destination: null,
        outcome: null,
        detail: `${record.count} ${record.count === 1 ? "request" : "requests"} refused for volume (Puddle no longer does this)`,
      };
    case "rule_created":
    case "rule_expired":
    case "rule_deleted":
    case "rule_updated": {
      const detail =
        record.type === "rule_deleted"
          ? `${ruleLabel(record.rule)}, by ${record.actor}, ${record.reason}`
          : record.type === "rule_updated"
            ? `${ruleLabel(record.rule)}, by ${record.actor}`
            : `${ruleLabel(record.rule)}${record.type === "rule_created" ? `, by ${actorLabel(record.rule.created_by)}` : ""}`;
      return {
        ts: record.ts,
        workspace: record.rule.workspace_id,
        type,
        destination: record.rule.pattern,
        outcome: null,
        detail,
      };
    }
    case "request_rule_created":
    case "request_rule_deleted": {
      const detail =
        record.type === "request_rule_deleted"
          ? `${requestRuleLabel(record.rule)}, by ${record.actor}, ${record.reason}`
          : `${requestRuleLabel(record.rule)}, by ${actorLabel(record.rule.created_by)}`;
      return {
        ts: record.ts,
        workspace: record.rule.workspace_id,
        type,
        destination: record.rule.host,
        outcome: null,
        detail,
      };
    }
    case "rule_set_created":
    case "rule_set_updated":
    case "rule_set_deleted":
      return {
        ts: record.ts,
        workspace: null,
        type,
        destination: record.rule_set.name,
        outcome: null,
        detail: `user:${record.rule_set.id}, by ${record.actor}`,
      };
    case "rule_set_switched":
      return {
        ts: record.ts,
        workspace: record.workspace_id,
        type,
        destination: record.set_id,
        outcome: null,
        detail: `${switchLabel(record.enabled)} for ${record.workspace_id === null ? "every workspace" : `workspace ${record.workspace_id}`}, by ${record.actor}`,
      };
    case "rule_set_changed":
      return {
        ts: record.ts,
        workspace: null,
        type,
        destination: record.set_id,
        outcome: null,
        detail: [
          record.added.length > 0 ? `added ${record.added.join(", ")}` : "",
          record.removed.length > 0
            ? `removed ${record.removed.join(", ")}`
            : "",
        ]
          .filter((part) => part !== "")
          .join("; "),
      };
    case "system_managed_changed":
      return {
        ts: record.ts,
        workspace: record.workspace_id,
        type,
        destination: null,
        outcome: null,
        detail: [
          record.added.length > 0 ? `now for ${reasonList(record.added)}` : "",
          record.removed.length > 0
            ? `no longer for ${reasonList(record.removed)}`
            : "",
        ]
          .filter((part) => part !== "")
          .join("; "),
      };
    case "workspace_op":
      return {
        ts: record.ts,
        workspace: record.workspace_id,
        type,
        destination: null,
        outcome:
          record.outcome === "failed"
            ? { label: "Failed", tone: "deny" }
            : { label: "Succeeded", tone: "allow" },
        detail: workspaceOpDetail(record),
      };
    case "audit_trimmed":
      return {
        ts: record.ts,
        workspace: null,
        type,
        destination: null,
        outcome: null,
        detail: `${record.deleted_records} older records deleted`,
      };
    default:
      return unknownRecord(record);
  }
}

/** A record type this build doesn't know (Puddle is newer than the app): the row says so, and its
 * raw record is in the expanded view. */
function unknownRecord(record: never): RowView {
  const raw = record as {
    type?: unknown;
    ts?: unknown;
    workspace_id?: unknown;
  };
  const type = typeof raw.type === "string" ? raw.type : "unknown";
  return {
    ts: typeof raw.ts === "number" ? raw.ts : 0,
    workspace: typeof raw.workspace_id === "string" ? raw.workspace_id : null,
    type,
    destination: null,
    outcome: null,
    detail: `a record of type "${type}" that this version of Puddle's app can't describe; open the row to see it as it was written`,
  };
}

/** The record as the API sent it, indented, for the row's expanded view. */
export function rawJson(record: AuditRecord): string {
  return JSON.stringify(record, null, 2);
}

/** One export line: the record exactly as the API sent it. */
export function jsonLine(record: AuditRecord): string {
  return `${JSON.stringify(record)}\n`;
}

/** `puddle-activity-2026-10-07T12-00-00.jsonl`. */
export function exportName(now: number): string {
  return `puddle-activity-${new Date(now).toISOString().slice(0, 19).replaceAll(":", "-")}.jsonl`;
}
