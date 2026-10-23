// SPDX-License-Identifier: GPL-3.0-or-later
// What the Repositories section says about a workspace's checkouts, decided without the server:
// the words for a branch and for a checkout the list does not know, and which identity serves a
// listed repository. Every string in a listing came from the guest, so these only build text;
// the page renders it as text.
import type { components } from "#lib/api/schema.d.ts";
import { covering, type Identity } from "#lib/identities/model.ts";
import { parseRepoUrl } from "#lib/identities/repo.ts";
import { operationLabel, type Workspace } from "./model.ts";
import { MAX_REPOSITORIES } from "./repo-rows.ts";

export type Checkout = components["schemas"]["Checkout"];
export type CheckoutListing = components["schemas"]["Checkouts"];
export type ListEntry = components["schemas"]["WorkspaceRepository"];

/** The branch, or where a detached HEAD is, or that nothing is committed yet. */
export function branchLabel(checkout: Checkout): string {
  if (checkout.branch !== null) return checkout.branch;
  return checkout.head === null
    ? "no commits yet"
    : `detached at ${checkout.head}`;
}

/** What Puddle says for a checkout it could not read, or `null` when it could. */
export function unreadableLabel(checkout: Checkout): string | null {
  return checkout.unreadable === null
    ? null
    : `Puddle could not read this checkout: ${checkout.unreadable}`;
}

/** Whether the list entry's address and the checkout's own remote name different repositories. */
export function remoteDiffers(checkout: Checkout): boolean {
  if (checkout.entry === null || checkout.remote_url === null) return false;
  const entry = parseRepoUrl(checkout.entry.url);
  const remote = parseRepoUrl(checkout.remote_url);
  if (!entry.ok || !remote.ok)
    return checkout.entry.url !== checkout.remote_url;
  return (
    entry.host !== remote.host ||
    entry.owner !== remote.owner ||
    entry.repo !== remote.repo
  );
}

/** The identity of `identities` that serves the repository at `url`, or `null` when none does. */
export function servingIdentity(
  identities: readonly Identity[],
  url: string,
): Identity | null {
  const parsed = parseRepoUrl(url);
  return parsed.ok ? covering(identities, parsed.host, parsed.owner) : null;
}

/** The list entries the listing says are not on the disk, in list order. */
export function missingEntries(
  entries: readonly ListEntry[],
  listing: CheckoutListing | null,
): ListEntry[] {
  if (listing === null) return [];
  return entries.filter((e) => listing.missing.includes(e.directory));
}

/** What a stopped workspace's section says in place of a listing. */
export function stoppedWords(listing: CheckoutListing | null): string {
  return listing !== null && listing.checked_at !== null
    ? "The workspace is stopped. This is the last listing."
    : "Start the workspace to see its checkouts.";
}

/** Why "Add repository" is off for this workspace, or `null` when it is on. */
export function addProblem(
  workspace: Workspace,
  entries: number = workspace.repositories.length,
): string | null {
  if (workspace.busy !== null) {
    return `${operationLabel(workspace.busy)}: wait for it to finish.`;
  }
  if (workspace.status !== "running") {
    return "Start the workspace to add a repository.";
  }
  return entries >= MAX_REPOSITORIES
    ? `A workspace lists at most ${MAX_REPOSITORIES} repositories.`
    : null;
}

/** Why "Clone again" is off for this workspace, or `null` when it is on. */
export function cloneAgainProblem(workspace: Workspace): string | null {
  if (workspace.busy !== null) {
    return `${operationLabel(workspace.busy)}: wait for it to finish.`;
  }
  return workspace.status === "running"
    ? null
    : "Start the workspace to clone it again.";
}
