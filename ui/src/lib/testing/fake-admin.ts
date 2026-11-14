// SPDX-License-Identifier: AGPL-3.0-or-later
import type { AdminUser } from "#lib/admin/model.ts";

/** An active user with nothing running, for tests. */
export const user = (over: Partial<AdminUser> = {}): AdminUser => ({
  id: 2,
  name: "Sam",
  role: "user",
  state: "active",
  created_at: 1,
  last_sign_in: null,
  devices: 0,
  disk_used_mib: 0,
  workspaces_running: 0,
  workspaces_stopped: 0,
  ...over,
});
