// SPDX-License-Identifier: AGPL-3.0-or-later
// Which keys are the terminal's own: everything else goes to the shell, and the page
// cancels the browser's default for the keys it hands over.

export interface KeyLike {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  /** AltGr reports Ctrl and Alt together on Windows; it types a character. */
  altGraph?: boolean;
}

export type KeyAction =
  /** Copy the selection: Ctrl+Shift+C. */
  | "copy"
  /**
   * Paste: Ctrl+Shift+V everywhere, and Shift+Insert only where the webview raises a paste event
   * (Windows). WebKitGTK on Linux raises none for Shift+Insert and shows its own paste prompt.
   * The webview's paste event does the work.
   */
  | "paste"
  /** F6: focus leaves the terminal for the sub-tab row. */
  | "leave"
  /** To the shell; the browser's default for it is cancelled (Ctrl+R, Ctrl+W, F5, ...). */
  | "shell-cancel"
  /** To the shell, nothing else to do. */
  | "shell";

export function keyAction(k: KeyLike): KeyAction {
  const key = k.key.toLowerCase();
  if (k.ctrlKey && k.shiftKey && !k.altKey && key === "c") return "copy";
  if (k.ctrlKey && k.shiftKey && !k.altKey && key === "v") return "paste";
  if (k.shiftKey && !k.ctrlKey && !k.altKey && key === "insert") {
    return "paste";
  }
  if (key === "f6" && !k.ctrlKey && !k.altKey && !k.shiftKey) return "leave";
  if (k.altGraph) return "shell";
  if (k.ctrlKey || k.metaKey || k.altKey || /^f\d{1,2}$/.test(key)) {
    return "shell-cancel";
  }
  return "shell";
}
