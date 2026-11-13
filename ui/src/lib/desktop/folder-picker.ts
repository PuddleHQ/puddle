// SPDX-License-Identifier: AGPL-3.0-or-later
// The desktop app's native folder dialog. Only the app's main window may call it (its one Tauri
// permission, `dialog:allow-open`); a browser tab has no such bridge, so the page shows no
// button there. The plugin is loaded on first use, so the browser build never fetches it.

/** Whether the page runs inside the desktop app's window, which injects Tauri's bridge. */
export function inDesktopApp(scope: object = globalThis as object): boolean {
  return "__TAURI_INTERNALS__" in scope;
}

/**
 * Opens the operating system's folder dialog. Resolves to the chosen folder's absolute path, or
 * `null` when the user cancels. Rejects when the dialog cannot open.
 */
export async function pickFolder(title: string): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const chosen = await open({ directory: true, multiple: false, title });
  return typeof chosen === "string" && chosen !== "" ? chosen : null;
}
