// SPDX-License-Identifier: AGPL-3.0-or-later
// The one list of top-level sections. Later rows add a screen by adding an entry here (and a
// folder under src/routes), never by editing the layout.
import { isWelcome, stepFor } from "./welcome/steps.ts";

export interface NavItem {
  /** Route path, also the key. */
  href: string;
  /** Sidebar label and page title. */
  label: string;
  /** Show the pending-requests badge. */
  badge?: "pending";
  /** Listed only where the server has an admin screen for the signed-in owner. */
  owner?: boolean;
}

export const NAV: readonly NavItem[] = [
  { href: "/workspaces", label: "Workspaces" },
  {
    href: "/inbox",
    label: "Inbox",
    badge: "pending",
  },
  { href: "/rules", label: "Rules" },
  { href: "/activity", label: "Activity" },
  { href: "/identities", label: "Identities" },
  { href: "/settings", label: "Settings" },
  {
    href: "/admin",
    label: "Admin",
    owner: true,
  },
];

export const APP_NAME = "Puddle";

/** The section a path belongs to (`/rules/123` belongs to `/rules`). */
export function sectionFor(pathname: string): NavItem | undefined {
  return NAV.find(
    (item) => pathname === item.href || pathname.startsWith(`${item.href}/`),
  );
}

/**
 * The width a route's content gets inside the one page frame: `reading` for forms and prose,
 * `full` for tables, the terminal and everything inside a workspace (its tabs share one frame, so
 * the page does not jump as the tab changes; a form tab keeps a reading-width block).
 */
export type Frame = "reading" | "full";

const FULL = [
  /^\/welcome(\/|$)/,
  /^\/workspaces(\/|$)/,
  /^\/inbox(\/|$)/,
  /^\/rules(\/|$)/,
  /^\/activity(\/|$)/,
  /^\/settings\/(network-health|app-errors)(\/|$)/,
];

export function frameFor(pathname: string): Frame {
  return FULL.some((pattern) => pattern.test(pathname)) ? "full" : "reading";
}

/** `Inbox (3) - Puddle`: the document title, with the pending count when there is one. */
export function documentTitle(pathname: string, pending: number): string {
  if (isWelcome(pathname)) {
    return `${stepFor(pathname)?.label ?? "Welcome"} - ${APP_NAME}`;
  }
  const section = sectionFor(pathname);
  const label = section?.label ?? "Not found";
  const count =
    section?.badge === "pending" && pending > 0 ? ` (${pending})` : "";
  return `${label}${count} - ${APP_NAME}`;
}
