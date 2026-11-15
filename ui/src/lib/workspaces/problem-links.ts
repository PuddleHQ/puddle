// SPDX-License-Identifier: AGPL-3.0-or-later
// Where a background problem about a workspace sends you. The host names a problem with a key
// (`start-folder:<id>`, `clone:<id>:<entry>`), where `<id>` is the workspace's id; a missing start folder goes to the
// setting, every other problem about a workspace to the workspace's overview (which holds the
// Repositories section a failed clone shows in).
import type { components } from "#lib/api/schema.d.ts";

export type Problem = components["schemas"]["Problem"];

export interface ProblemLink {
  href: string;
  label: string;
}

/** The key of the problem that says the workspace's start folder does not exist. */
export const startFolderKey = (id: string): string => `start-folder:${id}`;

/** The problem about the workspace's missing start folder, if the host lists one. */
export function startFolderProblemOf(
  problems: readonly Problem[],
  id: string,
): Problem | undefined {
  return problems.find((p) => p.key === startFolderKey(id));
}

/** The key of the problem that says the workspace's browser editor server could not be installed. */
export const editorInstallKey = (id: string): string => `editor-install:${id}`;

/** The problem about the workspace's editor install, if the host lists one. */
export function editorInstallProblemOf(
  problems: readonly Problem[],
  id: string,
): Problem | undefined {
  return problems.find((p) => p.key === editorInstallKey(id));
}

/** The key of the problem that says the workspace's browser editor could not take a newer build. */
export const editorUpdateKey = (id: string): string => `editor-update:${id}`;

/** The problem about the workspace's editor update, if the host lists one. */
export function editorUpdateProblemOf(
  problems: readonly Problem[],
  id: string,
): Problem | undefined {
  return problems.find((p) => p.key === editorUpdateKey(id));
}

/** The key of the problem that names the workspace disks the workspace list does not name. */
export const ORPHAN_VOLUMES_KEY = "orphan-volumes";

/** The key of the problem that says `workspace`'s browser editor server runs as root. */
export const editorSessionUserKey = (workspace: string): string =>
  `editor-session-user:${workspace}`;

/** The problem about `workspace`'s editor running as root, if the host lists one. */
export function editorSessionUserProblemOf(
  problems: readonly Problem[],
  workspace: string,
): Problem | undefined {
  return problems.find((p) => p.key === editorSessionUserKey(workspace));
}

/** The page place for a problem about a workspace; `undefined` for one about nothing. */
export function problemLink(problem: Problem): ProblemLink | undefined {
  if (problem.key === ORPHAN_VOLUMES_KEY) {
    return { href: "/workspaces", label: "Restore a workspace" };
  }
  const workspace = problem.workspace;
  if (!workspace) return undefined;
  const base = `/workspaces/${encodeURIComponent(workspace)}`;
  if (problem.key === startFolderKey(workspace)) {
    return {
      href: `${base}/settings#start-folder`,
      label: "Change start folder",
    };
  }
  return { href: base, label: "Open workspace" };
}
