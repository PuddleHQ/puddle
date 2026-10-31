// SPDX-License-Identifier: GPL-3.0-or-later
// Where a background problem about a workspace sends you. The host names a problem with a key
// (`start-folder:<workspace>`, `clone:<workspace>:<directory>`); a missing start folder goes to the
// setting, every other problem about a workspace to the workspace's overview (which holds the
// Repositories section a failed clone shows in).
import type { components } from "#lib/api/schema.d.ts";

export type Problem = components["schemas"]["Problem"];

export interface ProblemLink {
  href: string;
  label: string;
}

/** The key of the problem that says `workspace`'s start folder does not exist. */
export const startFolderKey = (workspace: string): string =>
  `start-folder:${workspace}`;

/** The problem about `workspace`'s missing start folder, if the host lists one. */
export function startFolderProblemOf(
  problems: readonly Problem[],
  workspace: string,
): Problem | undefined {
  return problems.find((p) => p.key === startFolderKey(workspace));
}

/** The key of the problem that says `workspace`'s browser editor server could not be installed. */
export const editorInstallKey = (workspace: string): string =>
  `editor-install:${workspace}`;

/** The problem about `workspace`'s editor install, if the host lists one. */
export function editorInstallProblemOf(
  problems: readonly Problem[],
  workspace: string,
): Problem | undefined {
  return problems.find((p) => p.key === editorInstallKey(workspace));
}

/** The key of the problem that says `workspace`'s browser editor could not take a newer build. */
export const editorUpdateKey = (workspace: string): string =>
  `editor-update:${workspace}`;

/** The problem about `workspace`'s editor update, if the host lists one. */
export function editorUpdateProblemOf(
  problems: readonly Problem[],
  workspace: string,
): Problem | undefined {
  return problems.find((p) => p.key === editorUpdateKey(workspace));
}

/** The page place for a problem about a workspace; `undefined` for one about nothing. */
export function problemLink(problem: Problem): ProblemLink | undefined {
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
