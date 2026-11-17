// SPDX-License-Identifier: AGPL-3.0-or-later
// The inbox's keyboard and mouse shortcuts, as the shortcuts popover lists them. The page's key
// handler and the Allow and Deny buttons do what each line says.

/** One line per shortcut. */
export type InboxShortcutId =
  "allow" | "deny" | "next" | "previous" | "more" | "everywhere";

export interface InboxShortcut {
  id: InboxShortcutId;
  /** The keys or gesture, each drawn as a key cap: `["Ctrl", "click"]`. */
  keys: readonly string[];
  /** What it does, as the popover says it. */
  does: string;
  /** A single letter: works only while the Settings switch for the inbox keys is on. */
  singleKey: boolean;
}

export const INBOX_SHORTCUTS: readonly InboxShortcut[] = [
  {
    id: "allow",
    keys: ["A"],
    does: "Allow the marked request for its workspace",
    singleKey: true,
  },
  {
    id: "deny",
    keys: ["D"],
    does: "Deny the marked request for its workspace",
    singleKey: true,
  },
  {
    id: "next",
    keys: ["J"],
    does: "Mark the next request",
    singleKey: true,
  },
  {
    id: "previous",
    keys: ["K"],
    does: "Mark the previous request",
    singleKey: true,
  },
  {
    id: "more",
    keys: ["Enter"],
    does: "More choices for the marked request",
    singleKey: false,
  },
  {
    id: "everywhere",
    keys: ["Ctrl", "click"],
    does: "On Allow or Deny: allow or deny for all workspaces",
    singleKey: false,
  },
];
