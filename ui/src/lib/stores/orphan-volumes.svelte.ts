// SPDX-License-Identifier: AGPL-3.0-or-later
// The workspace disks Puddle keeps whose workspace its list does not name, for the workspace
// list. Puddle cannot tell from outside whether such a disk holds any files, so nothing here
// says so. Restoring one adds a list entry for it; no call here deletes a disk.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { live, type LiveSource } from "./live.svelte.ts";
import { readFailure } from "./read-failure.ts";

export type OrphanVolume = components["schemas"]["OrphanVolumeView"];
type StoreApi = Pick<ApiClient, "GET" | "POST">;

/** What a restore came to. `needsAddress`: the disk gave no repository address and none was sent. */
export type RestoreResult =
  | { ok: true; name: string }
  | { ok: false; message: string; needsAddress: boolean };

export class OrphanVolumes {
  /** The unclaimed disks; empty until the first read and when there are none. */
  volumes = $state.raw<OrphanVolume[]>([]);
  /** Calls to `load` in order, so a slow answer never overwrites a newer one. */
  #ticket = 0;
  readonly #api: StoreApi;
  readonly #source: LiveSource | undefined;

  constructor(
    api: StoreApi = defaultApi,
    source: LiveSource | undefined = live,
  ) {
    this.#api = api;
    this.#source = source;
  }

  /** Reads the list. A failed read leaves what is shown as it is: the notice is a courtesy. */
  async load(): Promise<void> {
    const ticket = ++this.#ticket;
    try {
      const { data } = await this.#api.GET("/api/orphan-volumes");
      if (ticket === this.#ticket && data) this.volumes = data.volumes;
    } catch {
      // The list stays as it was.
    }
  }

  /** Reads now and follows the host's changes; returns the function that stops listening. */
  start(): () => void {
    void this.load();
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        const type = (event as { type?: unknown } | null)?.type;
        if (type === "problems_changed" || type === "status_changed")
          void this.load();
      },
      resync: () => void this.load(),
    });
    return () => unsubscribe?.();
  }

  /** Restores one disk as a workspace; the disk leaves the list once the host says it is in. */
  async restore(id: string, repository?: string): Promise<RestoreResult> {
    const address = repository?.trim();
    try {
      const { data, error, response } = await this.#api.POST(
        "/api/orphan-volumes/{id}/restore",
        {
          params: { path: { id } },
          body: address ? { repository: address } : {},
        },
      );
      if (!data) {
        return {
          ok: false,
          message: readFailure(error, response),
          needsAddress: response?.status === 422,
        };
      }
      this.#ticket++;
      this.volumes = this.volumes.filter((v) => v.id !== id);
      return { ok: true, name: data.name };
    } catch (failure) {
      return {
        ok: false,
        message: failure instanceof Error ? failure.message : String(failure),
        needsAddress: false,
      };
    }
  }
}

export const orphanVolumes = new OrphanVolumes();
