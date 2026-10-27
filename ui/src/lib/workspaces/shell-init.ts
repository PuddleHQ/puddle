// SPDX-License-Identifier: GPL-3.0-or-later
// The stream events the Shell init tab acts on.

/** What `event` says about the shell init of `name`: it changed, Puddle restored it, or neither. */
export function shellInitEvent(
  event: unknown,
  name: string,
): "changed" | "restored" | null {
  if (typeof event !== "object" || event === null) return null;
  const { type, workspace } = event as { type?: unknown; workspace?: unknown };
  if (workspace !== name) return null;
  if (type === "workspace_shell_init_changed") return "changed";
  if (type === "workspace_shell_init_restored") return "restored";
  return null;
}
