// SPDX-License-Identifier: GPL-3.0-or-later
// Whether the first-run flow has been through, as the API keeps it (in Puddle's settings).
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";

export type FirstRun = components["schemas"]["FirstRun"];
export type Status = "loading" | "ready" | "failed";

type StoreApi = Pick<ApiClient, "GET" | "PUT" | "POST">;

export class FirstRunStore {
  state = $state.raw<FirstRun | null>(null);
  status = $state<Status>("loading");
  /** Windows' confirmation is open, or Puddle is waiting for it. */
  trusting = $state(false);
  /** What the last press of "Trust it now" has to add to the state, or why it did not work. */
  trustNote = $state<string | null>(null);

  readonly #api: StoreApi;

  constructor(api: StoreApi = defaultApi) {
    this.#api = api;
  }

  /** Reads the state; never throws. */
  async load(): Promise<void> {
    try {
      const { data } = await this.#api.GET("/api/first-run");
      if (data) {
        this.state = data;
        this.status = "ready";
      } else if (this.state === null) {
        this.status = "failed";
      }
    } catch {
      if (this.state === null) this.status = "failed";
    }
  }

  /**
   * Asks Puddle to trust its development certificate; on Windows the user answers a confirmation
   * meanwhile, which can take up to two minutes. One request at a time; never throws.
   */
  async trust(): Promise<void> {
    if (this.trusting) return;
    this.trusting = true;
    this.trustNote = null;
    try {
      const { data, error } = await this.#api.POST(
        "/api/dev-certificate/trust",
      );
      if (data) {
        const base = this.state;
        if (base)
          this.state = { ...base, dev_certificate: data.dev_certificate };
        this.trustNote = data.detail;
      } else {
        this.trustNote = error?.message ?? "Puddle refused that.";
      }
    } catch {
      this.trustNote = "Puddle's service isn't answering.";
    } finally {
      this.trusting = false;
    }
  }

  /** Records that the flow was finished or skipped; says why when Puddle could not keep that. */
  async complete(): Promise<{ ok: true } | { ok: false; message: string }> {
    try {
      const { data, error } = await this.#api.PUT("/api/first-run", {
        body: { completed: true },
      });
      if (!data) {
        return {
          ok: false,
          message:
            error?.error === "newer_settings"
              ? "These settings were saved by a newer Puddle. Update Puddle to change them."
              : (error?.message ?? "Puddle refused that."),
        };
      }
      this.state = data;
      this.status = "ready";
      return { ok: true };
    } catch {
      return { ok: false, message: "Puddle's service isn't answering." };
    }
  }
}

export const firstRun = new FirstRunStore();
