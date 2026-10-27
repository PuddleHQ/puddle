// SPDX-License-Identifier: GPL-3.0-or-later
// What deleting a workspace removes from Puddle's own records, beside what is on its disk: its own
// variables and secrets (a secret's value leaves the credential store too) and its rules. The delete
// check of the disk does not know these, so the screen counts them from the lists it already has.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import { counts, plural } from "#lib/environment/model.ts";
import { workspaceOf } from "#lib/rules/model.ts";

export interface Removed {
  variables: number;
  secrets: number;
  rules: number;
}

/**
 * The counts for the workspace called `name`, or `null` when either list could not be read (the
 * dialog then says nothing about them, rather than a wrong number).
 */
export async function readRemoved(
  name: string,
  api: Pick<ApiClient, "GET"> = defaultApi,
): Promise<Removed | null> {
  try {
    const [env, rules] = await Promise.all([
      api.GET("/api/workspaces/{id}/env", { params: { path: { id: name } } }),
      api.GET("/api/rules"),
    ]);
    if (!env.data || !rules.data) return null;
    const own = counts(
      env.data.variables.filter((v) => v.scope === "workspace"),
    );
    return {
      ...own,
      rules: rules.data.rules.filter((r) => workspaceOf(r) === name).length,
    };
  } catch {
    return null;
  }
}

/** "2 variables, 1 secret and 3 rules", or `null` when there is nothing of the three. */
export function removedText(removed: Removed | null): string | null {
  if (!removed) return null;
  const parts = [
    removed.variables > 0 ? plural(removed.variables, "variable") : null,
    removed.secrets > 0 ? plural(removed.secrets, "secret") : null,
    removed.rules > 0 ? plural(removed.rules, "rule") : null,
  ].filter((p): p is string => p !== null);
  if (parts.length === 0) return null;
  return parts.length === 1
    ? parts[0]!
    : `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}`;
}
