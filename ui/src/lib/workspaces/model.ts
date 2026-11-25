// SPDX-License-Identifier: AGPL-3.0-or-later
// What the workspace screens decide without the server: which buttons make sense for a state,
// the words for states and progress steps, and the checks a create form can make before it asks.
// Pure TypeScript, no DOM.
import type { components } from "#lib/api/schema.d.ts";
import { bytesLabel } from "#lib/audit/model.ts";

export type Workspace = components["schemas"]["Workspace"];
export type Status = components["schemas"]["WorkspaceStatus"];
export type Operation = components["schemas"]["WorkspaceOperation"];
export type Step = components["schemas"]["WorkspaceStep"];
export type DeleteCheck = components["schemas"]["DeleteCheck"];

/** States with no VM: the workspace can be started or deleted. */
export function isDown(status: Status): boolean {
  return (
    status === "created" ||
    status === "stopped" ||
    status === "crashed" ||
    status === "volume_missing"
  );
}

/** Nothing is changing the workspace right now. */
export const isIdle = (w: Workspace): boolean => w.busy === null;

export const canStart = (w: Workspace): boolean =>
  isIdle(w) && isDown(w.status);
export const canStop = (w: Workspace): boolean =>
  isIdle(w) && w.status === "running";
export const canAttach = canStop;
export const canDelete = (w: Workspace): boolean =>
  isIdle(w) && isDown(w.status);
export const canReclaim = (w: Workspace): boolean =>
  isIdle(w) && (w.status === "running" || w.status === "stopped");
/** The root disk goes with the sandbox, so a reset needs the workspace down. */
export const canResetRootDisk = (w: Workspace): boolean =>
  isIdle(w) && (w.status === "stopped" || w.status === "crashed");

/**
 * A root disk can grow while the workspace runs, or waits for its next start when it is down;
 * only a start or stop in progress (and any other operation) holds it back.
 */
export const canGrowRootDisk = (w: Workspace): boolean =>
  isIdle(w) &&
  w.root_disk_size_mib !== null &&
  w.status !== "starting" &&
  w.status !== "draining" &&
  w.status !== "volume_missing";

/**
 * The largest root disk size the form offers, in whole GiB. A limit of the form, not of the API
 * (which takes any `u32` of MiB and refuses what this computer cannot hold): it keeps a typo
 * from asking for terabytes.
 */
export const MAX_ROOT_DISK_GIB = 2047;

/** What the grow form offers first: ten GiB above the current size, in whole GiB. */
export const suggestedRootDiskGib = (w: Workspace): number =>
  Math.min(
    MAX_ROOT_DISK_GIB,
    Math.floor((w.root_disk_size_mib ?? 0) / 1024) + 10,
  );

/**
 * Why `text` (whole GiB) can't be the new root disk size of `w`, or `null` when it can. The
 * service has the last word: it also refuses a size this computer has no room for.
 */
export function rootDiskGrowProblem(text: string, w: Workspace): string | null {
  const shown = text.trim();
  if (!/^\d+$/.test(shown)) return "Enter a whole number of GiB.";
  const gib = Number(shown);
  const current = w.root_disk_size_mib ?? 0;
  if (gib * 1024 <= current)
    return `A root disk only grows: choose more than ${formatMib(current)}.`;
  if (gib > MAX_ROOT_DISK_GIB)
    return `Choose at most ${MAX_ROOT_DISK_GIB} GiB.`;
  return null;
}

const STATUS_WORDS: Record<Status, string> = {
  created: "Not started",
  starting: "Starting",
  running: "Running",
  draining: "Stopping",
  paused: "Paused",
  stopped: "Stopped",
  crashed: "Crashed",
  volume_missing: "Volume missing",
};

export const statusLabel = (status: Status): string => STATUS_WORDS[status];

/** The tone a status chip takes; never the only carrier of the meaning (the label says it too). */
export type Tone = "ok" | "busy" | "warn" | "bad" | "idle";

const STATUS_TONES: Record<Status, Tone> = {
  created: "idle",
  starting: "busy",
  running: "ok",
  draining: "busy",
  paused: "warn",
  stopped: "idle",
  crashed: "bad",
  volume_missing: "bad",
};

export const statusTone = (status: Status): Tone => STATUS_TONES[status];

const OPERATION_WORDS: Record<Operation, string> = {
  creating: "Creating",
  starting: "Starting",
  stopping: "Stopping",
  reclaiming: "Reclaiming space",
  resetting_root_disk: "Resetting the root disk",
  growing_root_disk: "Making the root disk bigger",
  cloning: "Cloning",
  deleting: "Deleting",
};

export const operationLabel = (op: Operation): string => OPERATION_WORDS[op];

const STEP_WORDS: Record<Step, string> = {
  preparing_volume: "Preparing the disk",
  pulling_image: "Downloading the image",
  unpacking_image: "Preparing the image",
  cloning: "Cloning the repository",
  starting: "Starting the workspace",
  syncing: "Syncing your settings",
  checking: "Checking for unsaved work",
  reclaiming: "Reclaiming free space",
  resetting_root_disk: "Resetting the root disk",
  growing_root_disk: "Making the root disk bigger",
  stopping: "Stopping the workspace",
  removing: "Removing the workspace",
  done: "Done",
  failed: "Failed",
};

export const stepLabel = (step: Step): string => STEP_WORDS[step];

/** How much of an image is done, as the card prints it: "120 MB of 480 MB, 25 %", or just "120 MB"
 * while the total is not known. `null` when the stream gave no count. */
export function byteCountText(
  bytes: { done: number; total: number | null } | null,
): string | null {
  if (!bytes) return null;
  if (bytes.total === null || bytes.total <= 0) return bytesLabel(bytes.done);
  const share = Math.min(100, Math.floor((bytes.done / bytes.total) * 100));
  return `${bytesLabel(bytes.done)} of ${bytesLabel(bytes.total)}, ${share} %`;
}

/** What a step takes long over, in words, for the steps people sit and wait through. */
export const STEP_HINTS: Partial<Record<Step, string>> = {
  pulling_image:
    "The first workspace downloads its image once; later ones reuse it.",
  unpacking_image:
    "Turning the download into the disk the workspace boots from.",
  starting:
    "Booting the workspace's virtual machine takes a minute or two, every time it starts.",
};

/** The ordered steps each operation goes through, to show "step 2 of 3". */
const SCRIPTS: Record<Operation, readonly Step[]> = {
  creating: ["preparing_volume", "pulling_image", "cloning"],
  starting: ["starting", "syncing"],
  stopping: ["stopping", "reclaiming"],
  reclaiming: ["reclaiming"],
  resetting_root_disk: ["resetting_root_disk"],
  growing_root_disk: ["growing_root_disk"],
  cloning: ["cloning"],
  deleting: ["checking", "removing"],
};

/**
 * The operation a step belongs to, for when the page hears the step before it knows what the
 * workspace is doing (events can beat the answer to the request). `null` where the step says
 * nothing: `done`, `failed`.
 */
export function operationOfStep(step: Step): Operation | null {
  // Only a create reports it, and it is not one of the numbered steps ("step 2 of 3").
  if (step === "unpacking_image") return "creating";
  // `reclaiming` is also the last step of a stop; seen alone it is taken as a reclaim.
  const found = (
    Object.entries(SCRIPTS) as [Operation, readonly Step[]][]
  ).filter(([, steps]) => steps.includes(step));
  return (
    found.find(([op]) => op === "reclaiming")?.[0] ?? found[0]?.[0] ?? null
  );
}

/** Where a step sits in its operation: `{ index: 2, of: 3 }`, or `null` for a step not in it. */
export function stepPosition(
  op: Operation,
  step: Step,
): { index: number; of: number } | null {
  const steps = SCRIPTS[op];
  const at = steps.indexOf(step);
  return at < 0 ? null : { index: at + 1, of: steps.length };
}

function looksLikeScp(url: string): boolean {
  if (url.includes("://") || /^http/i.test(url)) return false;
  const colon = url.indexOf(":");
  const slash = url.indexOf("/");
  if (colon <= 1) return false;
  return slash < 0 || colon < slash;
}

/** Whether `raw` is written as an SSH address: `ssh://...` or the `host:path` form of `scp`. */
export function looksLikeSsh(raw: string): boolean {
  const url = raw.trim();
  return /^ssh:\/\//i.test(url) || looksLikeScp(url);
}

/** The host and path of an SSH address (`git@github.com:acme/web.git`, `ssh://git@host:443/acme/web`),
 * or `null` when it has a password, a space or no path. The user name and port are not kept: the
 * repository is the same whichever way it is reached. */
export function sshAddress(raw: string): { host: string; path: string } | null {
  const url = raw.trim();
  if (/\s/.test(url)) return null;
  const parts = /^ssh:\/\//i.test(url)
    ? /^ssh:\/\/(?:[^@/:]+@)?([^@/:]+)(?::\d+)?\/(.+)$/i.exec(url)
    : /^(?:[^@/:]+@)?([^@/:]+):(.+)$/.exec(url);
  const host = parts?.[1];
  const path = parts?.[2]?.replace(/^\/+/, "");
  return host && path ? { host: host.toLowerCase(), path } : null;
}

/** Hosts whose Clone button copies `https://<user>@host/...`; there the bare user name is
 * dropped instead of refused. */
const BARE_USER_HOSTS = [
  "github.com",
  "gitlab.com",
  "bitbucket.org",
  "dev.azure.com",
  "visualstudio.com",
];

function takesBareUser(host: string): boolean {
  const name = (host.split(":")[0] ?? "").toLowerCase();
  return BARE_USER_HOSTS.some((h) => name === h || name.endsWith(`.${h}`));
}

/** `raw` trimmed, with the bare user name of a known host removed
 * (`https://org@dev.azure.com/org/p/_git/r` becomes `https://dev.azure.com/org/p/_git/r`).
 * A password, a token or a user name on another host is left in place for `repoUrlProblem`
 * to refuse. */
export function cleanRepoUrl(raw: string): string {
  const url = raw.trim();
  if (!/^https:\/\//i.test(url)) return url;
  const rest = url.slice(8);
  const end = rest.search(/[/?#]/);
  const authority = end < 0 ? rest : rest.slice(0, end);
  const tail = end < 0 ? "" : rest.slice(end);
  const at = authority.lastIndexOf("@");
  if (at < 0) return url;
  const user = authority.slice(0, at);
  const host = authority.slice(at + 1);
  if (user === "" || user.includes(":") || user.includes("@")) return url;
  if (!takesBareUser(host)) return url;
  return `https://${host}${tail}`;
}

const SSH_SHAPE =
  "That is not an SSH address Puddle can read. Use git@github.com:owner/repo.git or the repository's HTTPS URL.";
const SSH_HOST =
  "Puddle reads SSH addresses of GitHub, GitLab, Bitbucket and Azure DevOps. For another host, use the repository's HTTPS URL.";

/** The SSH names of the Git hosts Puddle serves over SSH, with the HTTPS host each one stands for
 * (`PathForm` in the proxy's `ssh_git/grammar.rs`; the host reads them the same way,
 * `ssh_address_as_https` in `puddle-store`). */
export const SSH_HOSTS: Record<string, string> = {
  "github.com": "github.com",
  "ssh.github.com": "github.com",
  "gitlab.com": "gitlab.com",
  "altssh.gitlab.com": "gitlab.com",
  "bitbucket.org": "bitbucket.org",
  "altssh.bitbucket.org": "bitbucket.org",
  "ssh.dev.azure.com": "dev.azure.com",
  "vs-ssh.visualstudio.com": "visualstudio.com",
};

/** The HTTPS address of the repository an SSH address names, or why it can't be read. A workspace
 * is cloned over HTTPS, so this is the address the create sends and the table lists. Azure DevOps
 * writes `v3/<organisation>/<project>/<repository>`. */
export function sshAsHttps(raw: string): { url: string } | { message: string } {
  const address = sshAddress(raw);
  if (address === null || /[\p{Cc}?#\\]/u.test(address.path)) {
    return { message: SSH_SHAPE };
  }
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

/** The reason a repository URL can't be cloned, or `null` when it can. */
export function repoUrlProblem(raw: string): string | null {
  const url = cleanRepoUrl(raw);
  if (url === "") return "Enter the repository's HTTPS URL.";
  if (looksLikeSsh(url)) {
    const converted = sshAsHttps(url);
    return "message" in converted
      ? converted.message
      : repoUrlProblem(converted.url);
  }
  if (/\s/.test(url)) return "The URL must not contain spaces.";
  if (!/^https:\/\//i.test(url)) return "Use an https:// URL.";
  const authority = url.slice(8).split(/[/?#]/)[0] ?? "";
  if (authority.includes("@"))
    return "Remove the user name and password from the URL; Puddle supplies credentials itself.";
  const host = authority.includes(":")
    ? authority.slice(0, authority.lastIndexOf(":"))
    : authority;
  if (host === "") return "The URL has no host.";
  return null;
}

const EMPTY_ADDRESS =
  "Enter the repository's address, such as owner/repo or https://github.com/owner/repo.";
const NOT_SHORTHAND =
  "Give owner/repo for a GitHub repository, or the repository's full address.";
/** True for the refusal of an address that may only be unfinished, so a form waits to show it. */
export const isIncompleteAddress = (message: string): boolean =>
  message === NOT_SHORTHAND;
const DOT_NAME = "The address can't have . or .. as a name in its path.";
const TRAILING_DOT = "Leave out the dot at the end of the host name.";

/**
 * The https:// address a repository field means, or why it names none. Typed without a scheme,
 * `host/path` means `https://host/path` and `owner/repo` means a GitHub repository; an SSH address
 * of a Git host means its HTTPS address. The result is what the form sends and shows, and
 * `repoUrlProblem` takes it. A dot name or a host with a trailing dot is refused: it would be one
 * more spelling of a repository that the repository table keeps once.
 */
export function resolveRepoAddress(
  raw: string,
): { url: string } | { message: string } {
  const typed = raw.trim();
  if (typed === "") return { message: EMPTY_ADDRESS };
  const head = typed.split("/")[0] ?? "";
  // `user:password@host/...` is a credential, not the `host:path` form of an SSH address.
  const credential = /^[^@:]*:[^@]*@/.test(head);
  let url = typed;
  if (!credential && looksLikeSsh(typed)) {
    const converted = sshAsHttps(typed);
    if ("message" in converted) return converted;
    url = converted.url;
  } else if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(typed)) {
    if (head === "." || head === "..") return { message: DOT_NAME };
    if (head.includes(".") || head.includes("@")) {
      url = `https://${typed}`;
    } else {
      const names = typed.replace(/\/+$/, "").split("/");
      if (names.length !== 2 || names.some((n) => n === ""))
        return { message: NOT_SHORTHAND };
      url = `https://github.com/${names.join("/")}`;
    }
  }
  const problem = repoUrlProblem(url);
  if (problem !== null) return { message: problem };
  const clean = cleanRepoUrl(url);
  const rest = clean.slice("https://".length).split(/[?#]/)[0] ?? "";
  const [authority = "", ...path] = rest.split("/");
  if (path.some((n) => n === "." || n === "..")) return { message: DOT_NAME };
  if (authority.split(":")[0]?.endsWith(".")) return { message: TRAILING_DOT };
  return { url: clean };
}

/** A workspace name suggested by a repository URL: its last path segment, as typed. */
export function suggestName(repoUrl: string): string {
  const path = repoUrl.trim().split(/[?#]/)[0] ?? "";
  const last = path.split("/").filter(Boolean).pop() ?? "";
  const name = last.replace(/\.git$/i, "");
  return [...name].slice(0, MAX_NAME_LENGTH).join("");
}

export const MIN_MEMORY_MIB = 256;
export const MAX_MEMORY_MIB = 1_048_576;

/** Reads a memory amount in GiB as typed; `null` means "use the default". */
export function parseMemoryGib(
  text: string,
): { ok: true; mib: number | null } | { ok: false; message: string } {
  const trimmed = text.trim();
  if (trimmed === "") return { ok: true, mib: null };
  const gib = Number(trimmed);
  if (!Number.isFinite(gib))
    return { ok: false, message: "Enter a number of GiB." };
  const mib = Math.round(gib * 1024);
  if (mib < MIN_MEMORY_MIB || mib > MAX_MEMORY_MIB)
    return {
      ok: false,
      message: `Use between ${MIN_MEMORY_MIB / 1024} GiB and ${MAX_MEMORY_MIB / 1024} GiB.`,
    };
  return { ok: true, mib };
}

/** `8 GiB`, `512 MiB`, `1.5 GiB`. */
export function formatMib(mib: number): string {
  if (mib < 1024) return `${mib} MiB`;
  const gib = mib / 1024;
  return `${Number.isInteger(gib) ? gib : gib.toFixed(1)} GiB`;
}

/** `6 GiB of 32 GiB used`, or just the size when what is used isn't known. */
export function diskLabel(w: Workspace): string {
  return w.disk_used_mib === null
    ? `${formatMib(w.disk_size_mib)} disk`
    : `${formatMib(w.disk_used_mib)} of ${formatMib(w.disk_size_mib)} used`;
}

export type DiskKind = components["schemas"]["DiskKind"];

/** The root disk's use in words, or `null` when the host cannot say (no sandbox yet). */
export function rootDiskLabel(w: Workspace): string | null {
  if (w.root_disk_size_mib === null) return null;
  return w.root_disk_used_mib === null
    ? `${formatMib(w.root_disk_size_mib)} root disk`
    : `${formatMib(w.root_disk_used_mib)} of ${formatMib(w.root_disk_size_mib)} used`;
}

/** Whether the host flagged this disk as at or over the warning threshold. */
export const diskWarned = (w: Workspace, kind: DiskKind): boolean =>
  w.disk_warnings.includes(kind);

/** Percent used, rounded down; `null` when the use or the size is not known. */
export function diskPercent(w: Workspace, kind: DiskKind): number | null {
  const used = kind === "root" ? w.root_disk_used_mib : w.disk_used_mib;
  const size = kind === "root" ? w.root_disk_size_mib : w.disk_size_mib;
  if (used === null || size === null || size === 0) return null;
  return Math.min(100, Math.floor((used * 100) / size));
}

/** What the user calls them orders the list; the store sorts again after a rename. */
export function sortWorkspaces(list: readonly Workspace[]): Workspace[] {
  return [...list].sort(
    (a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id),
  );
}

/** Longest name, in characters (the API counts characters, not bytes). */
export const MAX_NAME_LENGTH = 60;

/**
 * The reason `text` can't be a workspace's name, or `null` when it can: the name is display text,
 * so only empty, too long, a control character and another workspace's name (ignoring case; the
 * API refuses it too: 409) are refused. `exceptId` is the workspace being renamed, which may keep
 * its own name in other case.
 */
export function nameProblem(
  text: string,
  exceptId: string | null,
  all: readonly Workspace[],
): string | null {
  const shown = text.trim();
  if (shown === "") return "Give the workspace a name.";
  if ([...shown].length > MAX_NAME_LENGTH)
    return `Use at most ${MAX_NAME_LENGTH} characters.`;
  if (/\p{Cc}/u.test(shown))
    return "Use a single line, without control characters.";
  const clash = all.find(
    (w) => w.id !== exceptId && w.name.toLowerCase() === shown.toLowerCase(),
  );
  return clash ? `Another workspace is already called "${clash.name}".` : null;
}

/** A rename goes through while the workspace is settled; only the name changes, so up or down is fine. */
export const canRename = (w: Workspace): boolean =>
  w.busy !== "creating" && w.busy !== "deleting";

/** What a delete would lose, as sentences for the confirm dialog. */
export interface LossLine {
  /** The checkout's directory; empty for what is outside any. */
  where: string;
  what: string;
  items: string[];
  more: number;
}

export function losses(check: DeleteCheck): LossLine[] {
  const lines: LossLine[] = [];
  for (const repo of check.repos) {
    const parts: [string, typeof repo.uncommitted][] = [
      ["Uncommitted changes", repo.uncommitted],
      ["Commits that are on no remote branch", repo.unpushed],
      ["Stashes", repo.stashes],
    ];
    for (const [what, list] of parts) {
      if (list.items.length > 0 || list.more > 0)
        lines.push({
          where: repo.dir,
          what,
          items: list.items,
          more: list.more,
        });
    }
  }
  if (check.other.items.length > 0 || check.other.more > 0)
    lines.push({
      where: "",
      what: "Files outside any repository",
      items: check.other.items,
      more: check.other.more,
    });
  return lines;
}
