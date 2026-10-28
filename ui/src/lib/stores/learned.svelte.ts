// SPDX-License-Identifier: GPL-3.0-or-later
// The hosts Puddle stopped decrypting in one workspace because a tool there refused Puddle's
// certificate, and the limits that say how long and how many. The list is read again on the event
// that announces a new host and on a resync; a change shows what the API answers.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { asPassThroughLearned } from "#lib/interception/learned.ts";
import { sentence } from "#lib/rules/model.ts";
import { live, type LiveSource } from "./live.svelte.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

export type LearnedHost = components["schemas"]["LearnedHost"];
export type Limits = components["schemas"]["LearnedLimits"];
export type Status = "loading" | "ready" | "failed";
export type Result = { ok: true } | { ok: false; message: string };
type StoreApi = Pick<ApiClient, "GET" | "PUT" | "DELETE">;

const NONE: LearnedHost[] = [];

export class LearnedList {
  status = $state<Status>("loading");
  hosts = $state.raw<LearnedHost[]>(NONE);
  reason = $state<string | null>(null);
  #id = "";
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

  /** Reads the list. `quiet` keeps what is shown when the read fails. */
  async load(id: string, quiet = false): Promise<void> {
    this.#id = id;
    if (!quiet) {
      this.status = "loading";
      this.reason = null;
    }
    const ticket = ++this.#ticket;
    try {
      const { data, error, response } = await this.#api.GET(
        "/api/workspaces/{id}/interception/learned",
        { params: { path: { id } } },
      );
      if (ticket !== this.#ticket) return;
      if (data) {
        this.hosts = data.hosts;
        this.status = "ready";
      } else if (!quiet) {
        this.status = "failed";
        this.reason = readFailure(error, response);
      }
    } catch {
      if (ticket !== this.#ticket || quiet) return;
      this.status = "failed";
      this.reason = SERVICE_DOWN;
    }
  }

  /** Reads `id`'s list now and keeps it current; returns the function that stops listening. */
  start(id: string): () => void {
    void this.load(id);
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        if (asPassThroughLearned(event)?.workspace === id) {
          void this.load(id, true);
        }
      },
      resync: () => void this.load(id, true),
    });
    return () => unsubscribe?.();
  }

  /** Decrypts one host again from the next connection. A host already gone counts as done. */
  async decryptAgain(host: string): Promise<Result> {
    return this.#delete(
      this.#api.DELETE("/api/workspaces/{id}/interception/learned/{host}", {
        params: { path: { id: this.#id, host } },
      }),
    );
  }

  /** Decrypts every learned host again. */
  async decryptAll(): Promise<Result> {
    return this.#delete(
      this.#api.DELETE("/api/workspaces/{id}/interception/learned", {
        params: { path: { id: this.#id } },
      }),
    );
  }

  async #delete(call: ReturnType<StoreApi["DELETE"]>): Promise<Result> {
    try {
      const { error, response } = await call;
      if (!response.ok && response.status !== 404) {
        return { ok: false, message: readFailure(error, response) };
      }
      await this.load(this.#id, true);
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }
}

/** How long and how many hosts Puddle passes through after a tool refused its certificate. */
export class LearnedLimitsStore {
  status = $state<Status>("loading");
  limits = $state.raw<Limits>({ ttl_secs: 43_200, cap: 1024 });
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
        "/api/interception/learned-limits",
      );
      if (ticket !== this.#ticket) return;
      if (data) {
        this.limits = data;
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

  async save(limits: Limits): Promise<Result> {
    const ticket = ++this.#ticket;
    try {
      const { data, error } = await this.#api.PUT(
        "/api/interception/learned-limits",
        { body: limits },
      );
      if (!data) {
        return {
          ok: false,
          message: sentence(error?.message ?? "Puddle refused the limits"),
        };
      }
      if (ticket === this.#ticket) this.limits = data;
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }
}
