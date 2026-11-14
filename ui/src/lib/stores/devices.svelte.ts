// SPDX-License-Identifier: AGPL-3.0-or-later
// The browsers signed in as the current user, the way to sign one out, and the sign-in methods.
// Nothing here is a credential: a device is a name and some times.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { sentence } from "#lib/rules/model.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

export type Device = components["schemas"]["DeviceView"];
export type MethodKind = components["schemas"]["SignInMethodKind"];
export type MethodView = components["schemas"]["SignInMethodView"];
export type Status = "loading" | "ready" | "failed";
export type Result = { ok: true } | { ok: false; message: string };
type StoreApi = Pick<ApiClient, "GET" | "PUT" | "DELETE">;

const NONE: Device[] = [];
const NO_METHODS: MethodView[] = [];

export class DevicesStore {
  status = $state<Status>("loading");
  devices = $state.raw<Device[]>(NONE);
  reason = $state<string | null>(null);
  /** The sign-in methods, when the caller may read them (the owner). */
  methods = $state.raw<MethodView[]>(NO_METHODS);
  #ticket = 0;
  readonly #api: StoreApi;

  constructor(api: StoreApi = defaultApi) {
    this.#api = api;
  }

  async load(): Promise<void> {
    const ticket = ++this.#ticket;
    this.reason = null;
    const [devices, methods] = await Promise.all([
      this.#readDevices(),
      this.#readMethods(),
    ]);
    // A newer read started meanwhile: its answer is the one to show.
    if (ticket !== this.#ticket) return;
    this.methods = methods;
    if ("devices" in devices) {
      this.devices = devices.devices;
      this.status = "ready";
    } else {
      this.status = "failed";
      this.reason = devices.reason;
    }
  }

  async #readDevices(): Promise<{ devices: Device[] } | { reason: string }> {
    try {
      const { data, error, response } = await this.#api.GET("/api/devices");
      return data
        ? { devices: data.devices }
        : { reason: readFailure(error, response) };
    } catch {
      return { reason: SERVICE_DOWN };
    }
  }

  /** The sign-in methods, or none for someone who may not read them. */
  async #readMethods(): Promise<MethodView[]> {
    try {
      const { data } = await this.#api.GET("/api/auth/methods");
      return data?.methods ?? NO_METHODS;
    } catch {
      return NO_METHODS;
    }
  }

  /** Signs a device out. */
  async revoke(id: number): Promise<Result> {
    try {
      const { error, response } = await this.#api.DELETE("/api/devices/{id}", {
        params: { path: { id } },
      });
      if (response.ok) {
        this.devices = this.devices.filter((device) => device.id !== id);
        return { ok: true };
      }
      // A device that is gone already is as good as revoked.
      if (response.status === 404) {
        await this.load();
        return { ok: true };
      }
      return { ok: false, message: readFailure(error, response) };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }

  /** Changes the signed-in user's password; every other browser is signed out. */
  async changePassword(current: string, next: string): Promise<Result> {
    try {
      const { error, response } = await this.#api.PUT("/api/auth/password", {
        body: { current, new: next },
      });
      if (response.ok) {
        await this.load();
        return { ok: true };
      }
      return {
        ok: false,
        message: error?.message
          ? sentence(error.message)
          : "Puddle refused that.",
      };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }

  /** Switches a sign-in method on or off; the service refuses to leave none on. */
  async setMethod(method: MethodKind, enabled: boolean): Promise<Result> {
    try {
      const { data, error } = await this.#api.PUT("/api/auth/methods", {
        body: { method, enabled },
      });
      if (data) {
        this.methods = data.methods;
        return { ok: true };
      }
      return {
        ok: false,
        message: error?.message
          ? sentence(error.message)
          : "Puddle refused that.",
      };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }
}
