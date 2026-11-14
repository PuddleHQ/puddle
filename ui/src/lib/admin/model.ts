// SPDX-License-Identifier: AGPL-3.0-or-later
// What the admin screen shows, in words: sizes, account states and audit lines. Nothing here
// handles a credential except `inviteLink`, which only puts the one-time secret into the link.
import type { components } from "#lib/api/schema.d.ts";

export type AdminUser = components["schemas"]["AdminUser"];
export type AdminStats = components["schemas"]["AdminStats"];
export type AuditLine = components["schemas"]["AdminAuditLine"];
export type InviteIssued = components["schemas"]["InviteIssued"];

/** A size in MiB as people read it: `512 MiB`, `1.5 GiB`. */
export function sizeText(mib: number): string {
  if (mib < 1024) return `${mib} MiB`;
  const gib = mib / 1024;
  return `${Number.isInteger(gib) ? gib : gib.toFixed(1)} GiB`;
}

/** `2 running, 1 stopped`, or `none`. */
export function workspacesText(user: AdminUser): string {
  const parts: string[] = [];
  if (user.workspaces_running > 0)
    parts.push(`${user.workspaces_running} running`);
  if (user.workspaces_stopped > 0)
    parts.push(`${user.workspaces_stopped} stopped`);
  return parts.length === 0 ? "none" : parts.join(", ");
}

/** The label of an account's state. */
export function stateText(user: AdminUser): string {
  switch (user.state) {
    case "invited":
      return "Invited";
    case "active":
      return "Active";
    case "deleting":
      return "Being deleted";
  }
}

/** Whether the owner may delete or re-invite this account from the screen (the API checks too). */
export function canDelete(user: AdminUser): boolean {
  return user.role !== "owner" && user.state !== "deleting";
}

/** The invite button's label: a forgotten password is a new invite for an active account. */
export function inviteLabel(user: AdminUser): string {
  return user.state === "invited" ? "New invite link" : "Reset password";
}

/** The link the person opens; the secret is after `#`, so no server log or referrer sees it. */
export function inviteLink(origin: string, secret: string): string {
  return `${origin}/invite#${secret}`;
}

/** Whether the address in use is one the invited person cannot reach. */
export function isLoopback(origin: string): boolean {
  try {
    const { hostname } = new URL(origin);
    return (
      hostname === "localhost" ||
      hostname === "127.0.0.1" ||
      hostname === "[::1]" ||
      hostname.endsWith(".localhost")
    );
  } catch {
    return false;
  }
}

/** One audit line as a sentence: who did what to whom. */
export function auditText(line: AuditLine): string {
  switch (line.action) {
    case "user_created":
      return `${line.actor} made the account ${line.target}`;
    case "invite_reissued":
      return `${line.actor} gave ${line.target} a new invite link`;
    case "user_delete_started":
      return `${line.actor} started deleting ${line.target}`;
    case "user_deleted":
      return `${line.target} was deleted`;
  }
}
