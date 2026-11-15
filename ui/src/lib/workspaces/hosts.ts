// SPDX-License-Identifier: AGPL-3.0-or-later
// Which hosts the repositories of a create form are on, and where each stands: the answer of
// `POST /api/repository-hosts`. `new` hosts are the ones the create will allow for the workspace;
// `blocked` ones (a deny rule covers them) refuse the create.
import { api } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";

export type RepositoryHost = components["schemas"]["RepositoryHost"];

/** Asks for the hosts of `urls`; `null` when the service could not answer (the form then says nothing). */
export type HostCheck = (
  urls: string[],
  workspace?: string,
) => Promise<RepositoryHost[] | null>;

export const checkHosts: HostCheck = async (urls, workspace) => {
  try {
    const { data } = await api.POST("/api/repository-hosts", {
      body: { urls, ...(workspace ? { workspace } : {}) },
    });
    return data?.hosts ?? null;
  } catch {
    return null;
  }
};

/** The hosts a create would allow, as one phrase: "github.com", "a and b", "a, b and c". */
export function hostList(hosts: readonly string[]): string {
  if (hosts.length <= 1) return hosts[0] ?? "";
  return `${hosts.slice(0, -1).join(", ")} and ${hosts[hosts.length - 1]}`;
}

/** The notice after a create or an add that allowed hosts, or `null` when it allowed none. */
export function allowedNotice(
  hosts: readonly { host: string }[],
): string | null {
  if (hosts.length === 0) return null;
  const names = hostList(hosts.map((h) => h.host));
  return `Puddle allowed ${names} for this workspace so it can clone. The rule${hosts.length === 1 ? " is" : "s are"} on the Rules screen, marked as made by the create.`;
}
