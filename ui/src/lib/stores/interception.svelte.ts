// SPDX-License-Identifier: GPL-3.0-or-later
// How a workspace decrypts hosts: the switch for decrypting every host and the names it never
// decrypts, plus the global never-decrypt list. Each change is saved at once and the screen shows
// what the API answers, never what was typed.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

type View = components["schemas"]["InterceptionView"];
type StoreApi = Pick<ApiClient, "GET" | "PUT">;

export type Status = "loading" | "ready" | "failed";
export type SaveResult = { ok: true } | { ok: false; message: string };

const NO_NAMES: string[] = [];

export class WorkspaceInterception {
  status = $state<Status>("loading");
  decryptAll = $state(false);
  /** The names this workspace never decrypts. */
  names = $state.raw<string[]>(NO_NAMES);
  /** The names no workspace decrypts; changed on the global settings page. */
  globalNames = $state.raw<string[]>(NO_NAMES);
  reason = $state<string | null>(null);
  #id = "";
  /** Calls in order, so a slow answer never overwrites a newer one. */
  #ticket = 0;
  readonly #api: StoreApi;

  constructor(api: StoreApi = defaultApi) {
    this.#api = api;
  }

  #show(view: View): void {
    this.decryptAll = view.decrypt_all;
    this.names = view.never_decrypt;
    this.globalNames = view.global_never_decrypt;
  }

  async load(id: string): Promise<void> {
    this.#id = id;
    this.status = "loading";
    this.reason = null;
    const ticket = ++this.#ticket;
    try {
      const { data, error, response } = await this.#api.GET(
        "/api/workspaces/{id}/interception",
        { params: { path: { id } } },
      );
      if (ticket !== this.#ticket) return;
      if (data) {
        this.#show(data);
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

  setDecryptAll(on: boolean): Promise<SaveResult> {
    return this.#put({ decrypt_all: on });
  }

  saveNames(names: string[]): Promise<SaveResult> {
    return this.#put({ never_decrypt: names });
  }

  async #put(
    body: components["schemas"]["InterceptionRequest"],
  ): Promise<SaveResult> {
    const ticket = ++this.#ticket;
    try {
      const { data, error, response } = await this.#api.PUT(
        "/api/workspaces/{id}/interception",
        { params: { path: { id: this.#id } }, body },
      );
      if (!data) return { ok: false, message: readFailure(error, response) };
      if (ticket === this.#ticket) this.#show(data);
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }
}

/** The names no workspace decrypts. */
export class GlobalNeverDecrypt {
  status = $state<Status>("loading");
  names = $state.raw<string[]>(NO_NAMES);
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
        "/api/interception/never-decrypt",
      );
      if (ticket !== this.#ticket) return;
      if (data) {
        this.names = data.never_decrypt;
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

  async save(names: string[]): Promise<SaveResult> {
    const ticket = ++this.#ticket;
    try {
      const { data, error, response } = await this.#api.PUT(
        "/api/interception/never-decrypt",
        { body: { never_decrypt: names } },
      );
      if (!data) return { ok: false, message: readFailure(error, response) };
      if (ticket === this.#ticket) this.names = data.never_decrypt;
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }
}
