// SPDX-License-Identifier: AGPL-3.0-or-later
// The repository a clone address names, in the one spelling a workspace's repository table keeps
// (host and names lower-case, no `.git`, Azure DevOps `project/repo`, names decoded: `My%20Project`
// is `my project`). The host reads addresses the same way (`RepoRef::from_https_url`); both are
// tested against one list of cases.
import {
  looksLikeSsh,
  repoUrlProblem,
  sshAddress,
} from "#lib/workspaces/model.ts";

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

const SSH_SHAPE =
  "That is not an SSH address Puddle can read. Use git@github.com:owner/repo.git or the repository's HTTPS URL.";
const SSH_HOST =
  "Puddle reads SSH addresses of GitHub, GitLab, Bitbucket and Azure DevOps. For another host, use the repository's HTTPS URL.";

/** The SSH names of the Git hosts Puddle serves over SSH, with the HTTPS host each one stands for
 * (`PathForm` in the proxy's `ssh_git/grammar.rs`). */
const SSH_HOSTS: Record<string, string> = {
  "github.com": "github.com",
  "ssh.github.com": "github.com",
  "gitlab.com": "gitlab.com",
  "altssh.gitlab.com": "gitlab.com",
  "bitbucket.org": "bitbucket.org",
  "altssh.bitbucket.org": "bitbucket.org",
  "ssh.dev.azure.com": "dev.azure.com",
  "vs-ssh.visualstudio.com": "visualstudio.com",
};

/** The HTTPS address of the repository an SSH address names, or why it can't be read. Azure DevOps
 * writes `v3/<organisation>/<project>/<repository>`. */
function sshAsHttps(raw: string): { url: string } | { message: string } {
  const address = sshAddress(raw);
  if (address === null) return { message: SSH_SHAPE };
  const https = SSH_HOSTS[address.host];
  if (https === undefined) return { message: SSH_HOST };
  if (
    https === "github.com" ||
    https === "gitlab.com" ||
    https === "bitbucket.org"
  ) {
    return { url: `https://${https}/${address.path}` };
  }
  const [version, org, project, repo, ...more] = address.path.split("/");
  if (version !== "v3" || !org || !project || !repo || more.length > 0) {
    return { message: SSH_SHAPE };
  }
  return https === "dev.azure.com"
    ? { url: `https://dev.azure.com/${org}/${project}/_git/${repo}` }
    : { url: `https://${org}.visualstudio.com/${project}/_git/${repo}` };
}

/** The repository `raw` names, or why it can't be listed. With `ssh`, an SSH address names the
 * repository the same way its HTTPS address does and is listed under the HTTPS host (the table
 * holds repositories, not ways to reach them); the create form leaves it off, because a workspace
 * is still cloned over HTTPS. */
export function parseRepoUrl(
  raw: string,
  options: { ssh?: boolean } = {},
): ParsedRepo {
  if (options.ssh === true && looksLikeSsh(raw)) {
    const converted = sshAsHttps(raw);
    if ("message" in converted)
      return { ok: false, message: converted.message };
    return parseRepoUrl(converted.url);
  }
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
