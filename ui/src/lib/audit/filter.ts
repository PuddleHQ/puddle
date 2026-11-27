// SPDX-License-Identifier: AGPL-3.0-or-later
// The activity screen's filters: what they default to, how they turn into an API query and a page
// address, and the words for their choices. Pure: nothing here touches the page or the network.
import type { components } from "#lib/api/schema.d.ts";

type Schemas = components["schemas"];
export type AuditType = Schemas["AuditType"];
export type AuditOutcome = Schemas["AuditOutcome"];
type CompanySignal = Schemas["CompanySignal"];

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
export const COMPANY_SIGNAL_WORDS: Record<CompanySignal, string> = {
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
  { value: "rule_switched", label: "Rule switched" },
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
