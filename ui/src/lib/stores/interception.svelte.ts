// SPDX-License-Identifier: AGPL-3.0-or-later
// How a workspace decrypts hosts: the switch for decrypting every host and the names it never
// decrypts, plus the global never-decrypt list. Each change is saved at once and the screen shows
// what the API answers, never what was typed.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { INTERCEPTION_NOTICE_KEY } from "#lib/notify/interception.ts";
import { notices } from "#lib/notify/notices.svelte.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

type View = components["schemas"]["InterceptionView"];
export type WorkspaceMode = View["mode"];
export type EffectiveMode = View["effective_mode"];
type StoreApi = Pick<ApiClient, "GET" | "PUT">;

export type Status = "loading" | "ready" | "failed";
export type SaveResult = { ok: true } | { ok: false; message: string };

const NO_NAMES: string[] = [];

export class WorkspaceInterception {
  status = $state<Status>("loading");
  /** The workspace's own choice; "inherit" follows the global setting. */
  mode = $state<WorkspaceMode>("inherit");
  /** The mode in force: the workspace's own, or the global one. */
  effectiveMode = $state<EffectiveMode>("known_only");
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
    this.mode = view.mode;
    this.effectiveMode = view.effective_mode;
    this.names = view.never_decrypt;
    this.globalNames = view.global_never_decrypt;
  }

  /** Reads the workspace's settings. `quiet` keeps what is shown while it reads and when it fails. */
  async load(id: string, quiet = false): Promise<void> {
    this.#id = id;
    if (!quiet) {
      this.status = "loading";
      this.reason = null;
    }
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

  setMode(mode: WorkspaceMode): Promise<SaveResult> {
    return this.#put({ mode });
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

type SettingsView = components["schemas"]["InterceptionSettingsView"];

/** The global setting: the mode workspaces follow, learning, the curated list and the notice. */
export class GlobalInterception {
  status = $state<Status>("loading");
  /** The mode in force; an install that has not chosen is on `known_only`. */
  mode = $state<SettingsView["mode"]>("known_only");
  /** Whether the user has chosen a mode. */
  chosen = $state(false);
  learn = $state(true);
  /** The built-in list mode 2 passes through ("System managed"). */
  curated = $state.raw<string[]>(NO_NAMES);
  reason = $state<string | null>(null);
  readonly #api: StoreApi;

  constructor(api: StoreApi = defaultApi) {
    this.#api = api;
  }

  #show(view: SettingsView): void {
    this.mode = view.mode;
    this.chosen = view.chosen;
    this.learn = view.learn_from_rejections;
    this.curated = view.curated;
  }

  async load(): Promise<void> {
    this.status = "loading";
    this.reason = null;
    try {
      const { data, error, response } =
        await this.#api.GET("/api/interception");
      if (data) {
        this.#show(data);
        this.status = "ready";
      } else {
        this.status = "failed";
        this.reason = readFailure(error, response);
      }
    } catch {
      this.status = "failed";
      this.reason = SERVICE_DOWN;
    }
  }

  setMode(mode: SettingsView["mode"]): Promise<SaveResult> {
    return this.#put({ mode });
  }

  setLearning(on: boolean): Promise<SaveResult> {
    return this.#put({ learn_from_rejections: on });
  }

  async #put(
    body: components["schemas"]["InterceptionSettingsRequest"],
  ): Promise<SaveResult> {
    try {
      const { data, error, response } = await this.#api.PUT(
        "/api/interception",
        {
          body,
        },
      );
      if (!data) return { ok: false, message: readFailure(error, response) };
      this.#show(data);
      this.status = "ready";
      // Any answer settles the one-time notice.
      if (data.chosen || data.notice_seen)
        notices.resolve(INTERCEPTION_NOTICE_KEY);
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }
}

/**
 * Skipping the first-run flow takes the recommended mode, unless one was chosen already. Best
 * effort: if it fails the install stays unchosen and the notice offers the choice later.
 */
export async function takeRecommendedMode(
  api: StoreApi = defaultApi,
): Promise<void> {
  const store = new GlobalInterception(api);
  await store.load();
  if (store.status === "ready" && !store.chosen) {
    await store.setMode("all_except_curated");
  }
}
