// SPDX-License-Identifier: AGPL-3.0-or-later
// The stream events the Shell init tab acts on.

/** What `event` says about the shell init of workspace `id`: it changed, Puddle restored it, or neither. */
export function shellInitEvent(
  event: unknown,
  id: string,
): "changed" | "restored" | null {
  if (typeof event !== "object" || event === null) return null;
  const { type, workspace } = event as { type?: unknown; workspace?: unknown };
  if (workspace !== id) return null;
  if (type === "workspace_shell_init_changed") return "changed";
  if (type === "workspace_shell_init_restored") return "restored";
  return null;
}
