// SPDX-License-Identifier: AGPL-3.0-or-later
// How big the activity log is, how fast it grows and for how many days it is kept. A save answers
// with the numbers as they are now, so the card needs no second read.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { AuditSize } from "#lib/audit/retention.ts";
import { sentence } from "#lib/rules/model.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

export type Status = "loading" | "ready" | "failed";
export type Result = { ok: true } | { ok: false; message: string };
type StoreApi = Pick<ApiClient, "GET" | "PUT">;

export class AuditRetentionStore {
  status = $state<Status>("loading");
  size = $state.raw<AuditSize | null>(null);
  reason = $state<string | null>(null);
  #ticket = 0;
  readonly #api: StoreApi;

  constructor(api: StoreApi = defaultApi) {
    this.#api = api;
  }

  async load(): Promise<void> {
    this.status = "loading";
    this.reason = null;
    const ticket = ++this.#ticket;
    try {
      const { data, error, response } = await this.#api.GET(
        "/api/audit/retention",
      );
      if (ticket !== this.#ticket) return;
      if (data) {
        this.size = data;
        this.status = "ready";
      } else {
        this.status = "failed";
        this.reason = readFailure(error, response);
      }
    } catch {
      if (ticket !== this.#ticket) return;
      this.status = "failed";
      this.reason = SERVICE_DOWN;
    }
  }

  /** Keeps the log for `days`; the server's refusal, in its words, comes back as the message. */
  async save(days: number): Promise<Result> {
    const ticket = ++this.#ticket;
    try {
      const { data, error } = await this.#api.PUT("/api/audit/retention", {
        body: { days },
      });
      if (!data) {
        return {
          ok: false,
          message: sentence(error?.message ?? "Puddle refused that"),
        };
      }
      if (ticket === this.#ticket) {
        this.size = data;
        this.status = "ready";
      }
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }
}
