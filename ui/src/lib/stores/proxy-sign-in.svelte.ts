// SPDX-License-Identifier: AGPL-3.0-or-later
// The sign-ins Puddle holds for company proxies that ask for a user name and password. The list
// names proxies and users only: the password goes out once and is never read back, and is not
// kept here after the call.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

export type StoredSignIn = components["schemas"]["StoredSignIn"];
export type Status = "loading" | "ready" | "failed";
export type Result = { ok: true } | { ok: false; message: string };
type StoreApi = Pick<ApiClient, "GET" | "PUT" | "DELETE">;

const NONE: StoredSignIn[] = [];

export class ProxySignInStore {
  status = $state<Status>("loading");
  signIns = $state.raw<StoredSignIn[]>(NONE);
  /** Proxies the network report knows, to offer when typing one. */
  suggestions = $state.raw<string[]>([]);
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
        "/api/proxy-sign-ins",
      );
      if (ticket !== this.#ticket) return;
      if (data) {
        this.signIns = data.sign_ins;
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
    await this.#suggest();
  }

  /** The proxies the network report names; a report that can't be read just offers none. */
  async #suggest(): Promise<void> {
    try {
      const { data } = await this.#api.GET("/api/network-health");
      if (!data) return;
      const named = [
        data.proxy.http_proxy,
        data.proxy.https_proxy,
        ...data.sign_in.attempts.map((a) => a.proxy),
        ...data.proxy.dead_proxies.map((d) => d.proxy),
      ];
      const names = named.filter(
        (name, at): name is string => !!name && named.indexOf(name) === at,
      );
      this.suggestions = names.sort();
    } catch {
      // Nothing to offer; the field still takes a typed address.
    }
  }

  /** Stores the sign-in, replacing an earlier one for the same proxy. */
  async save(proxy: string, user: string, password: string): Promise<Result> {
    const ticket = ++this.#ticket;
    try {
      const { data, error, response } = await this.#api.PUT(
        "/api/proxy-sign-ins",
        { body: { proxy, user, password } },
      );
      if (!data) return { ok: false, message: readFailure(error, response) };
      if (ticket === this.#ticket) this.signIns = data.sign_ins;
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }

  /** Forgets the sign-in for `proxy`. */
  async remove(proxy: string): Promise<Result> {
    try {
      const { error, response } = await this.#api.DELETE(
        "/api/proxy-sign-ins/{proxy}",
        { params: { path: { proxy } } },
      );
      if (!response.ok)
        return { ok: false, message: readFailure(error, response) };
      this.signIns = this.signIns.filter((s) => s.proxy !== proxy);
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }
}
