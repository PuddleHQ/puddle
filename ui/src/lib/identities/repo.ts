// SPDX-License-Identifier: GPL-3.0-or-later
// The repository a clone address names, in the one spelling a workspace's repository table keeps
// (host and names lower-case, no `.git`, Azure DevOps `project/repo`, names decoded: `My%20Project`
// is `my project`). The host reads addresses the same way (`RepoRef::from_https_url`); both are
// tested against one list of cases.
import { repoUrlProblem } from "#lib/workspaces/model.ts";

export interface RepoRef {
  host: string;
  owner: string;
  repo: string;
}

export type ParsedRepo =
  ({ ok: true } & RepoRef) | { ok: false; message: string };

const SHAPE =
  "That is not a repository address Puddle can read. Use https://github.com/owner/repo or https://dev.azure.com/organisation/project/_git/repo.";

const NAME = /^[a-z0-9._-]{1,100}$/;
// An Azure DevOps project or repository name: any letter or space, but nothing that ends or splits
// a path segment, no control character, and no space at either end.
const AZURE_FORBIDDEN = /[\p{Cc}/\\%?#;]/u;

/** Folds ASCII letters only, as the host does: `K` (the Kelvin sign) is not `k` to every Git host. */
function asciiLower(text: string): string {
  return text.replace(/[A-Z]/g, (c) => c.toLowerCase());
}

/** How many names a repository's path holds at most: the host's own rule (`RepoRef::max_segments`). */
function maxNames(host: string): number {
  if (host === "dev.azure.com" || host.endsWith(".visualstudio.com")) return 2;
  return host === "github.com" ? 1 : 21;
}

function azureName(part: string): boolean {
  return (
    part !== "" &&
    part === part.trim() &&
    [...part].length <= 100 &&
    part !== "." &&
    part !== ".." &&
    !AZURE_FORBIDDEN.test(part)
  );
}

/** One address segment with its `%XX` escapes decoded; `null` when it is not one an address can
 * have (a raw space or control character, a bad escape, one that is not UTF-8, a space at an
 * end, or an escape for what separates segments). */
function decodeSegment(raw: string): string | null {
  if (/[\p{Cc} ]/u.test(raw)) return null;
  let decoded: string;
  try {
    decoded = decodeURIComponent(raw);
  } catch {
    return null;
  }
  if (decoded !== decoded.trim()) return null;
  // `%` only reaches here as `%25`, which a segment may not decode to.
  return /[\p{Cc}/\\%?#;]/u.test(decoded) ? null : decoded;
}

function refuse(): ParsedRepo {
  return { ok: false, message: SHAPE };
}

/** The repository `raw` names, or why it can't be listed. */
export function parseRepoUrl(raw: string): ParsedRepo {
  const problem = repoUrlProblem(raw);
  if (problem !== null) return { ok: false, message: problem };
  const rest = raw.trim().slice("https://".length).split(/[?#]/)[0] ?? "";
  const slash = rest.indexOf("/");
  if (slash < 0) return refuse();
  const host = asciiLower(rest.slice(0, slash));
  if (host.includes(":") || host.includes("@")) return refuse();
  const decoded = rest
    .slice(slash + 1)
    .split("/")
    .filter((s) => s !== "")
    .map(decodeSegment);
  if (decoded.some((s) => s === null)) return refuse();
  const segments = decoded as string[];
  const isGit = (s: string | undefined) => s?.toLowerCase() === "_git";
  let owner: string;
  let repo: string;
  const [a, b, c, d] = segments;
  if (host === "dev.azure.com") {
    if (segments.length === 3 && a && isGit(b) && c) {
      owner = a;
      repo = `${c}/${c}`;
    } else if (segments.length === 4 && a && b && isGit(c) && d) {
      owner = a;
      repo = `${b}/${d}`;
    } else return refuse();
  } else if (host.endsWith(".visualstudio.com")) {
    owner = host.slice(0, -".visualstudio.com".length);
    if (segments.length === 3 && a && isGit(b) && c) repo = `${a}/${c}`;
    else if (segments.length === 4 && b && isGit(c) && d) repo = `${b}/${d}`;
    else return refuse();
  } else if (host !== "github.com" && segments.length >= 2 && a) {
    // Subgroups (GitLab) are part of the name.
    owner = a;
    repo = segments.slice(1).join("/");
  } else if (segments.length === 2 && a && b) {
    owner = a;
    repo = b;
  } else return refuse();
  owner = asciiLower(owner);
  repo = asciiLower(repo).replace(/\.git$/, "");
  const parts = repo.split("/");
  const azure = host === "dev.azure.com" || host.endsWith(".visualstudio.com");
  const valid =
    NAME.test(owner) &&
    parts.length >= 1 &&
    parts.length <= maxNames(host) &&
    parts.every((p) =>
      azure
        ? azureName(p)
        : NAME.test(p) && p !== "." && p !== ".." && p !== "-",
    );
  return valid ? { ok: true, host, owner, repo } : refuse();
}

/** `github.com/acme/web-shop`, as a row of the table reads. */
export function repoName(repo: RepoRef): string {
  return `${repo.host}/${repo.owner}/${repo.repo}`;
}
