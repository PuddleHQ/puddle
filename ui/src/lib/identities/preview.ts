// SPDX-License-Identifier: GPL-3.0-or-later
// Which identity a new workspace's repository will get, worked out in the form before anything is
// sent. It mirrors the host's choice (`choose` in the store): the identity you picked, else one that
// names the owner, else one that covers the rest of the host (one already on the workspace's list
// first), else the default identity, else none. The host stays the authority; this only tells.
import type { Identity } from "./model.ts";

export type Basis = "chosen" | "covers" | "default" | "none";

export interface Preview {
  /** The identity the repository will get, or `null` when there is none. */
  identity: Identity | null;
  basis: Basis;
  /** Whether the identity has a credential for the repository's owner, so requests are signed. */
  covered: boolean;
  /** One sentence for the form. */
  text: string;
}

/** How an identity covers an owner on a host: by naming it, as the rest of the host, or not. */
function level(
  identity: Identity,
  host: string,
  owner: string,
): "owner" | "rest" | null {
  const on = identity.credentials.filter(
    (c) => c.host.toLowerCase() === host.toLowerCase(),
  );
  if (on.some((c) => c.covers.owners.includes(owner.toLowerCase())))
    return "owner";
  return on.some((c) => c.covers.rest_of_host) ? "rest" : null;
}

/**
 * What the host will give a repository at `host`/`owner`. `chosen` is the identity picked in the
 * form (`null`: Puddle chooses); `onList` are the identities earlier repositories of the same
 * workspace already brought, which win a tie.
 */
export function previewIdentity(
  everyone: readonly Identity[],
  onList: readonly Identity[],
  chosen: number | null,
  host: string,
  owner: string,
): Preview {
  const where = `${owner} on ${host}`;
  const picked =
    chosen === null ? undefined : everyone.find((i) => i.id === chosen);
  if (picked) {
    const covered = level(picked, host, owner) !== null;
    return {
      identity: picked,
      basis: "chosen",
      covered,
      text: covered
        ? `Uses ${picked.label}: it covers ${where}.`
        : `Uses ${picked.label}, which has no credential for ${where}: it signs the commits, but requests go out without a credential.`,
    };
  }
  for (const wanted of ["owner", "rest"] as const) {
    for (const pool of [onList, everyone]) {
      const found = pool.find((i) => level(i, host, owner) === wanted);
      if (found)
        return {
          identity: found,
          basis: "covers",
          covered: true,
          text: `Uses ${found.label}: it covers ${where}.`,
        };
    }
  }
  const fallback = everyone.find((i) => i.is_default);
  if (fallback)
    return {
      identity: fallback,
      basis: "default",
      covered: false,
      text: `No identity covers ${where}, so Puddle uses the default, ${fallback.label}. It signs the commits, but requests go out without a credential. Add one on the Identities tab.`,
    };
  return {
    identity: null,
    basis: "none",
    covered: false,
    text: `No identity covers ${where} and you have no default identity, so this workspace has no commit author and no credential. Add an identity on the Identities tab.`,
  };
}
