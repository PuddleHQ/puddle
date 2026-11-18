// SPDX-License-Identifier: AGPL-3.0-or-later
// What an upgrade must keep, as a pure comparison (no network, no process): the dumps of the GET
// calls taken on the older build and on the newer one, reduced to what a user would call "the same"
// (ids, names, counts, values), and the version relation of the two builds. upgrade-state.mjs
// calls it; ui/scripts/upgrade-compare.test.ts holds the breakages each rule must catch.

/** The GET calls dumped before the upgrade and compared after it (the playbook's S2.7c list, plus the environment). */
export const DUMPED = ["/api/workspaces", "/api/rules", "/api/pending", "/api/settings", "/api/identities", "/api/env"];

/** Keys whose value legitimately differs after a restart or an upgrade, at any depth: the last read of a credential. */
const VOLATILE = new Set(["last_check", "checked_at"]);

const byId = (list) => [...list].sort((a, b) => Number(a.id) - Number(b.id) || String(a.id).localeCompare(String(b.id)));

/**
 * The part of one dump an upgrade must keep. Workspaces keep their id and name (their status changes
 * with the quit); the pending list keeps what was asked (the attempt count and times move with the
 * guest's retries); settings keep the saved sections, not the values worked out from them (a newer
 * build may change a default); the rest is kept whole.
 */
export function project(path, body) {
  switch (path) {
    case "/api/workspaces":
      return { count: body.workspaces.length, workspaces: byId(body.workspaces).map(({ id, name }) => ({ id, name })) };
    case "/api/pending":
      return {
        count: body.requests.length,
        requests: byId(body.requests).map(({ id, workspace, host, port, state }) => ({ id, workspace, host, port, state })),
      };
    case "/api/rules":
      return { count: body.rules.length, rules: byId(body.rules) };
    case "/api/settings": {
      const { workspace_defaults, vscode_server, ui, tools, limits } = body;
      return { workspace_defaults, vscode_server, ui, tools, limits };
    }
    case "/api/identities":
      return { count: body.identities.length, identities: byId(body.identities) };
    case "/api/env":
      return { count: body.variables.length, variables: [...body.variables].sort((a, b) => a.name.localeCompare(b.name)) };
    default:
      if (/^\/api\/workspaces\/[^/]+\/env$/.test(path)) {
        return { count: body.variables.length, variables: [...body.variables].sort((a, b) => a.name.localeCompare(b.name)) };
      }
      return body;
  }
}

/**
 * Every difference where `after` lacks or changes something `before` has. A key only `after` has is a
 * field a newer build added, which is fine; a list must keep its length (a lost row is a loss).
 */
export function missing(before, after, at = "") {
  if (before === null || typeof before !== "object") {
    return before === after ? [] : [`${at || "(root)"}: was ${JSON.stringify(before)}, is ${JSON.stringify(after)}`];
  }
  if (after === null || typeof after !== "object") return [`${at || "(root)"}: was ${JSON.stringify(before).slice(0, 80)}, is ${JSON.stringify(after)}`];
  if (Array.isArray(before)) {
    if (!Array.isArray(after)) return [`${at}: was a list, is not`];
    if (before.length !== after.length) return [`${at}: ${before.length} entries became ${after.length}`];
    return before.flatMap((item, i) => missing(item, after[i], `${at}[${i}]`));
  }
  return Object.keys(before)
    .filter((key) => !VOLATILE.has(key))
    .flatMap((key) => (key in after ? missing(before[key], after[key], `${at}.${key}`) : [`${at}.${key}: was ${JSON.stringify(before[key]).slice(0, 80)}, is gone`]));
}

/** The losses of one dump, named by the call that made it. */
export function compareDump(path, before, after) {
  return missing(project(path, before), project(path, after)).map((loss) => `GET ${path} ${loss}`);
}

/** The losses of every dumped call; `before` and `after` map a path to the parsed body. */
export function compareDumps(before, after) {
  return Object.keys(before).flatMap((path) => (path in after ? compareDump(path, before[path], after[path]) : [`GET ${path}: not dumped after the upgrade`]));
}

/** `1.2.3` or `1.2.3-pre+build` as numbers; anything else is NaN so a comparison with it fails loudly. */
export function versionParts(text) {
  const match = /^(\d+)\.(\d+)\.(\d+)/.exec(String(text));
  return match ? match.slice(1).map(Number) : [Number.NaN, Number.NaN, Number.NaN];
}

/** -1, 0 or 1; NaN when either is no version. */
export function compareVersions(a, b) {
  const x = versionParts(a);
  const y = versionParts(b);
  if (x.some(Number.isNaN) || y.some(Number.isNaN)) return Number.NaN;
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  return 0;
}

/**
 * Whether the build now running is not older than the one before, by what `GET /api/health` says.
 * Every develop build reports 0.0.0, so the version only rules out a step back; the installer's own
 * package version (checked by msi-install-test.ps1) says it is newer. Returns the problems.
 */
export function buildIsNotOlder(before, after) {
  const problems = [];
  if (!(compareVersions(before.version, after.version) <= 0)) problems.push(`health.version went from ${before.version} to ${after.version}`);
  if (!(compareVersions(before.api_version, after.api_version) <= 0)) problems.push(`health.api_version went from ${before.api_version} to ${after.api_version}`);
  return problems;
}

/** The names a page shows when it failed to load, as the UI's error page and banners word them. */
export const ERROR_TEXT = /something went wrong|page not found|unknown error/i;
