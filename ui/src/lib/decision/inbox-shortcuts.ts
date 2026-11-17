// SPDX-License-Identifier: AGPL-3.0-or-later
// The inbox's keyboard and mouse shortcuts, as the shortcuts popover lists them. The page's key
// handler and the Allow and Deny buttons do what each line says, and the inbox tests (unit and
// end-to-end) press every line of this table and check the result, so a line that stops being
// true fails a test. Only what works goes here: a row is no Tab stop, and Enter on a button
// presses that button, so "Tab to a request, then Enter" is not a way to more choices.

/** One line per shortcut; the tests key their proofs by it. */
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
    does: "More choices for the request that has focus (J, K or a click focuses one)",
    singleKey: false,
  },
  {
    id: "everywhere",
    keys: ["Ctrl", "click"],
    does: "On Allow or Deny: allow or deny for all workspaces",
    singleKey: false,
  },
];
