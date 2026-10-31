// SPDX-License-Identifier: GPL-3.0-or-later
// The create form's list of repositories without the DOM: which directory an address gets, which
// rows clash, how a start folder is checked, and which row a refusal of the service is about.
// The service checks all of this again; these checks only tell the user sooner, in the same words.
import { repoUrlProblem } from "./model.ts";

/** The most repositories one workspace lists (the service refuses more). */
export const MAX_REPOSITORIES = 100;
/** The longest checkout directory, in characters. */
export const MAX_DIRECTORY_LENGTH = 100;
/** The directory an address that names no usable folder is cloned into. */
export const FALLBACK_DIRECTORY = "src";

/** The reason `name` can't be a checkout directory, or `null` when it can. */
export function directoryProblem(name: string): string | null {
  if (name === "") return "Give the directory a name.";
  if (name.length > MAX_DIRECTORY_LENGTH)
    return `Use at most ${MAX_DIRECTORY_LENGTH} characters.`;
  if (name.startsWith(".") || name.startsWith("-"))
    return "Don't start with a dot or a hyphen.";
  if (name === "lost+found") return "lost+found is the disk's own folder.";
  if (!/^[A-Za-z0-9._-]+$/.test(name))
    return "Use only ASCII letters, digits, dots, underscores and hyphens.";
  return null;
}

/**
 * The directory `git clone <url>` would make: the last part of the address without `.git`, or
 * `src` when that is no usable name. The same rule as the service's, so the form can show it.
 */
export function directoryOf(url: string): string {
  const trimmed = url.trim().replace(/\/+$/, "");
  const last = trimmed.split(/[/:]/).pop() ?? "";
  const name = last.endsWith(".git") ? last.slice(0, -4) : last;
  return directoryProblem(name) === null ? name : FALLBACK_DIRECTORY;
}

/**
 * The folder name of a checkout on this computer: the last part of its path (the part before
 * `.git` when the path names the `.git` itself), or `null` when that is no usable directory.
 */
export function folderOf(path: string): string | null {
  const parts = path
    .trim()
    .split(/[\\/]+/)
    .filter(Boolean);
  if (parts[parts.length - 1] === ".git") parts.pop();
  const name = parts.pop() ?? "";
  return directoryProblem(name) === null ? name : null;
}

/** The reason `path` can't name a checkout on this computer, or `null` when it can. */
export function localPathProblem(path: string): string | null {
  const p = path.trim();
  if (p === "") return "Give the folder of the checkout.";
  if (!/^(\/|[A-Za-z]:[\\/]|\\\\)/.test(p))
    return "Give the full path of the folder, such as /home/you/project or C:\\Users\\you\\project.";
  return null;
}

/** One row of the form as the user left it. */
export interface RowDraft {
  /** Stable identity of the row, for ids and keyed rendering. */
  key: number;
  /** Where the code comes from: cloned from `url`, or a checkout on this computer. */
  source: "address" | "folder";
  /**
   * With `source` `"folder"`: the checkout's folder on this computer. An empty `url` then takes
   * the checkout's own `origin`.
   */
  localPath: string;
  url: string;
  /** What was typed in the directory field; empty follows the address. */
  directory: string;
  /** The identity picked by hand: `null` follows the form's choice, `""` is "chosen by Puddle". */
  identity: string | null;
  /** The identity that listed the address in the picker, while the address is still as picked. */
  listedBy: number | null;
  pull: boolean;
  push: boolean;
}

export function newRow(key: number, url = ""): RowDraft {
  return {
    key,
    source: "address",
    localPath: "",
    url,
    directory: "",
    identity: null,
    listedBy: null,
    pull: true,
    push: true,
  };
}

/**
 * The directory the row will be cloned into: what was typed, else the checkout's folder name
 * when it starts from a folder, else what the address gives.
 */
export function directoryFor(
  row: Pick<RowDraft, "url" | "directory"> &
    Partial<Pick<RowDraft, "source" | "localPath">>,
): string {
  const typed = row.directory.trim();
  if (typed !== "") return typed;
  const local = row.source === "folder" ? (row.localPath?.trim() ?? "") : "";
  if (local !== "") return folderOf(local) ?? directoryOf(row.url);
  return directoryOf(row.url);
}

/** `<dir>-2`, `<dir>-3`, ...: the first that no row in `taken` uses. */
export function freeDirectory(dir: string, taken: readonly string[]): string {
  for (let n = 2; ; n++) {
    const suffix = `-${n}`;
    const candidate = `${dir.slice(0, MAX_DIRECTORY_LENGTH - suffix.length)}${suffix}`;
    if (!taken.includes(candidate)) return candidate;
  }
}

/** An address reduced to how the repository table spells it: host and path, no `.git`, any case. */
export function sameRepositoryKey(url: string): string | null {
  try {
    const parsed = new URL(url.trim());
    if (parsed.protocol !== "https:") return null;
    const path = parsed.pathname.replace(/\/+$/, "").replace(/\.git$/i, "");
    return `${parsed.host}${path}`.toLowerCase();
  } catch {
    return null;
  }
}

export interface RowProblems {
  localPath?: string;
  url?: string;
  directory?: string;
}

/**
 * The problems a row has that depend on the other rows: a directory two rows share, with a free
 * one to use instead, and the same repository listed twice. A row with no address and no directory
 * typed is left alone, so a fresh row does not clash with the first.
 */
export function clashes(rows: readonly RowDraft[]): Map<number, RowProblems> {
  const found = new Map<number, RowProblems>();
  const set = (key: number, problems: RowProblems) =>
    found.set(key, { ...found.get(key), ...problems });
  const dirs = rows.map((r) =>
    r.url.trim() === "" &&
    r.directory.trim() === "" &&
    (r.source === "address" || r.localPath.trim() === "")
      ? null
      : directoryFor(r),
  );
  const taken = dirs.filter((d): d is string => d !== null);
  rows.forEach((row, at) => {
    const dir = dirs[at];
    if (dir == null) return;
    const other = dirs.findIndex((d, i) => i !== at && d === dir);
    if (other < 0) return;
    const place = other < at ? "earlier" : "later";
    set(row.key, {
      directory: `Repository ${other + 1}, the ${place} one, also goes into ${dir}. Use another directory, such as ${freeDirectory(dir, taken)}.`,
    });
  });
  const keys = rows.map((r) => sameRepositoryKey(r.url));
  rows.forEach((row, at) => {
    const key = keys[at];
    if (key == null) return;
    const first = keys.indexOf(key);
    if (first < at) {
      set(row.key, {
        url: `This is the same repository as repository ${first + 1}. List it once.`,
      });
    }
  });
  return found;
}

/**
 * What is wrong with each row on its own, by row key: an address the create refuses, a typed
 * directory that is no directory name, and a list over the limit (at its last row).
 */
export function rowProblems(
  rows: readonly RowDraft[],
): Map<number, RowProblems> {
  const found = new Map<number, RowProblems>();
  const add = (key: number, problems: RowProblems) =>
    found.set(key, { ...found.get(key), ...problems });
  for (const row of rows) {
    const fromFolder = row.source === "folder";
    const local = fromFolder ? localPathProblem(row.localPath) : null;
    // From a folder, the address may stay empty: the checkout's origin gives it.
    const url =
      fromFolder && row.url.trim() === "" ? null : repoUrlProblem(row.url);
    const typed = row.directory.trim();
    const dir = typed === "" ? null : directoryProblem(typed);
    if (local) add(row.key, { localPath: local });
    if (url) add(row.key, { url });
    if (dir) add(row.key, { directory: dir });
  }
  const last = rows[rows.length - 1];
  if (rows.length > MAX_REPOSITORIES && last)
    add(last.key, {
      url: `A workspace holds at most ${MAX_REPOSITORIES} repositories.`,
    });
  return found;
}

/** Everything that stops the list from being sent: `rowProblems` and `clashes` together. */
export function checkRows(rows: readonly RowDraft[]): Map<number, RowProblems> {
  const found = clashes(rows);
  for (const [key, problems] of rowProblems(rows))
    found.set(key, { ...found.get(key), ...problems });
  return found;
}

const START_FOLDER_MAX_BYTES = 255;

/**
 * The reason `text` can't be a start folder, or `null` when it can (empty is the workspace root).
 * A relative path under the volume: parts between `/`, none empty, `.` or `..`, no control
 * character or backslash, not inside `.puddle` or `lost+found`, at most 255 bytes.
 */
export function startFolderProblem(text: string): string | null {
  const path = text.trim();
  if (path === "") return null;
  if (new TextEncoder().encode(path).length > START_FOLDER_MAX_BYTES)
    return `Use at most ${START_FOLDER_MAX_BYTES} bytes.`;
  // eslint-disable-next-line no-control-regex -- a control character is what is refused
  if (/[\u0000-\u001f\u007f\\]/.test(path))
    return "Use / between folders, with no backslash or control character.";
  if (path.startsWith("/"))
    return "Give a path under the workspace root, without a leading /.";
  const parts = path.split("/");
  if (parts.some((p) => p === ""))
    return "A folder name between two / can't be empty.";
  if (parts.some((p) => p === "." || p === ".."))
    return "Leave out . and .. : the folder is a path down from the root.";
  const first = parts[0];
  if (first === ".puddle" || first === "lost+found")
    return `${first} belongs to Puddle and the disk; pick another folder.`;
  return null;
}

/** Which row of the list a refusal is about, from the service's words; `null` when none. */
export function refusedRow(
  message: string,
): { row: number; field: "localPath" | "url" | "directory" } | null {
  // "repositories 1 (a) and 2 (b) both go into api; ..." and "... are the same repository, ..."
  const pair = /^repositories (\d+) \(.*?\) and (\d+) \(/.exec(message);
  if (pair) {
    return {
      row: Number(pair[2]),
      field: /both go into/.test(message) ? "directory" : "url",
    };
  }
  const single = /^repository (\d+) \(/.exec(message);
  if (single && message.startsWith(`repository ${single[1]} (the checkout `))
    return { row: Number(single[1]), field: "localPath" };
  if (single) {
    // The service's reasons for a directory it refuses (`CheckoutDir::new`).
    const field = /lost\+found|ASCII letters|100 characters|`\.` or `-`/.test(
      message,
    )
      ? "directory"
      : "url";
    return { row: Number(single[1]), field };
  }
  return null;
}
