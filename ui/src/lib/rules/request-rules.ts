// SPDX-License-Identifier: GPL-3.0-or-later
// Rules on the method and path of a request as pure data: how one reads, the checks the form runs
// before it asks (the same ones the API applies, so a typing slip is caught at the field), and how
// the API's refusals map back to a field. No layout and no API calls here.
import type { components } from "#lib/api/schema.d.ts";

export type RequestRule = components["schemas"]["RequestRule"];
export type NewRequestRule = components["schemas"]["NewRequestRuleRequest"];

/** Most methods one rule names, and most segments or bytes a path pattern has. */
export const MAX_METHODS = 16;
export const MAX_PATH_SEGMENTS = 32;
export const MAX_PATH_BYTES = 512;

/** The two things the screen says about every rule of this kind (rules spec R-49, R-50). */
export const REQUEST_RULES_INTRO =
  "A rule here only refuses: it never opens a host. It names a host a workspace may reach and the methods and paths on it that are not allowed, such as DELETE on /repos/**. Puddle reads the requests to that host, so it decrypts the host while the rule exists, whatever the decrypt setting says.";

export const REQUEST_RULES_NOTE =
  "A rule applies from the next connection a workspace opens; a connection that is already open keeps judging by the rules it began with. It guards against an agent's mistakes, not against software set on evading it.";

/** "Every method" or "DELETE, PUT". */
export function methodsLabel(rule: Pick<RequestRule, "methods">): string {
  return rule.methods.length === 0 ? "Every method" : rule.methods.join(", ");
}

export function scopeLabel(rule: Pick<RequestRule, "workspace">): string {
  return rule.workspace ?? "Every workspace";
}

/** "refuse DELETE /repos/** on api.example.com for every workspace": how a rule is named. */
export function requestRuleName(rule: RequestRule): string {
  const methods =
    rule.methods.length === 0 ? "every method" : rule.methods.join(", ");
  const who =
    rule.workspace === null ? "every workspace" : `workspace ${rule.workspace}`;
  return `refuse ${methods} ${rule.path} on ${rule.host} for ${who}`;
}

/** Your rules in the order the screen shows them: host, then path, then id. */
export function requestRulesView(
  rules: readonly RequestRule[],
  workspace?: string,
): RequestRule[] {
  return [...rules]
    .filter(
      (rule) =>
        workspace === undefined ||
        rule.workspace === null ||
        rule.workspace === workspace,
    )
    .sort(
      (a, b) =>
        a.host.localeCompare(b.host) ||
        a.path.localeCompare(b.path) ||
        a.id - b.id,
    );
}

/** The workspaces the rules name, for the form's suggestions. */
export function requestRuleWorkspaces(rules: readonly RequestRule[]): string[] {
  return [
    ...new Set(
      rules.map((r) => r.workspace).filter((w): w is string => w !== null),
    ),
  ].sort();
}

const IPV4 = /^\d{1,3}(\.\d{1,3}){3}$/;

/** Why `host` is not a host name or `*.` pattern, or `null` when it may be one. */
export function hostProblem(host: string): string | null {
  const text = host.trim();
  if (text === "")
    return "Enter a host, such as api.example.com or *.example.com.";
  const name = text.startsWith("*.") ? text.slice(2) : text;
  if (/[\s/:?#@]/.test(name) || name.includes("*"))
    return "Enter a host name only: no scheme, port or path. A * may only start the name, as in *.example.com.";
  if (IPV4.test(name) || name.includes("["))
    return "Rules name hosts, not IP addresses.";
  if (text.startsWith("*.") && name.split(".").length < 2)
    return "A pattern needs at least two labels after *., such as *.example.com.";
  return null;
}

/** Why `path` is not a path pattern, or `null`: the same shape checks the API runs. */
export function pathProblem(path: string): string | null {
  if (path.trim() === "")
    return "Enter a path, such as /repos/** or /admin. Use /** for every path.";
  if (!path.startsWith("/")) return "A path starts with /, such as /repos/**.";
  if ([...path].some((c) => c.charCodeAt(0) < 0x20 || c === "\u007f"))
    return "A path holds no control characters.";
  if (new TextEncoder().encode(path).length > MAX_PATH_BYTES)
    return `A path is at most ${MAX_PATH_BYTES} bytes.`;
  if (/[?#]/.test(path))
    return "A path has no query string or fragment: rules never see them.";
  const segments = path.split("/").slice(1);
  const named = segments.filter((s) => s !== "");
  if (named.some((s) => s.includes("*") && s !== "*" && s !== "**"))
    return "A * stands for a whole segment (/repos/*/issues), not part of one.";
  const open = named.indexOf("**");
  if (open !== -1 && open !== named.length - 1)
    return "** may only end a path (/gists/**).";
  if (named.length > MAX_PATH_SEGMENTS)
    return `A path has at most ${MAX_PATH_SEGMENTS} segments.`;
  return null;
}

/** The method names typed in a box: split at commas and spaces, upper case, no repeats. */
export function splitMethods(text: string): string[] {
  return [
    ...new Set(
      text
        .split(/[\s,]+/)
        .filter((m) => m !== "")
        .map((m) => m.toUpperCase()),
    ),
  ];
}

/** Why the methods typed are not usable, or `null`. An empty box means every method. */
export function methodsProblem(text: string): string | null {
  const methods = splitMethods(text);
  const bad = methods.find(
    (m) => m.length > 32 || !/^[A-Z0-9!#$%&'*+\-.^_`|~]+$/.test(m),
  );
  if (bad !== undefined)
    return `"${bad.slice(0, 32)}" is not an HTTP method. Use names such as GET, POST or DELETE.`;
  if (methods.length > MAX_METHODS)
    return `Name at most ${MAX_METHODS} methods, or leave the box empty for every method.`;
  return null;
}

export type RequestField = "host" | "methods" | "path" | "workspace" | "form";

/**
 * Where an API refusal belongs: the API prefixes a bad method list with `methods:` and a bad
 * path with `path:`; the other refusals are about the host, except the per-scope limit.
 */
export function refusalField(message: string): {
  field: RequestField;
  message: string;
} {
  const lower = message.toLowerCase();
  for (const field of ["methods", "path", "workspace"] as const) {
    if (lower.startsWith(`${field}:`)) {
      return { field, message: message.slice(field.length + 1).trim() };
    }
  }
  if (lower.startsWith("at most ")) return { field: "form", message };
  return { field: "host", message };
}
