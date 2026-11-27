// SPDX-License-Identifier: AGPL-3.0-or-later
// The server's rule for which actions a workspace's state admits (`admits` and `actions` in
// crates/puddle-services/src/workspaces/phase.rs), for the test fakes only, so a fake workspace
// carries the `actions` the real API would send. The screens never decide this: they read
// `Workspace.actions`. Keep it in step with the Rust table test
// `the_actions_of_a_workspace_are_the_ones_its_phase_admits`.
import type { Action, Status, Workspace } from "#lib/workspaces/model.ts";

const DOWN: Status[] = ["created", "stopped", "crashed", "volume_missing"];

const ADMITS: Record<Action, (status: Status) => boolean> = {
  start: (s) => DOWN.includes(s),
  stop: (s) => s === "running",
  attach: (s) => s === "running",
  delete: (s) => DOWN.includes(s),
  reclaim: (s) => !["starting", "draining", "volume_missing"].includes(s),
  reset_root_disk: (s) => DOWN.includes(s),
  grow_root_disk: (s) => !["starting", "draining"].includes(s),
  add_repository: (s) => s === "running",
};

/** The actions the server would list for a workspace with this status and busy mark. */
export function actionsFor(
  status: Status,
  busy: Workspace["busy"],
): Workspace["actions"] {
  if (busy !== null) return [];
  return (Object.keys(ADMITS) as Action[]).filter((a) => ADMITS[a](status));
}

/** `w` with the actions its status and busy mark admit. */
export const withActions = (w: Workspace): Workspace => ({
  ...w,
  actions: actionsFor(w.status, w.busy),
});
