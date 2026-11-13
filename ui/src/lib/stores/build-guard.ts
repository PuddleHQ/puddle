// SPDX-License-Identifier: GPL-3.0-or-later
// Keeps a tab from running old code against a newer server. The server names the build of the app
// it serves in `GET /api/health`; a tab whose own build differs reloads, once per server build.
// A tab left open across an upgrade (or a server that came back after one) is the case: the
// event stream reconnecting is the cue, with a slow poll behind it for a stream that never broke.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { LiveSource } from "./live.svelte.ts";

/** Where the build already reloaded for is kept, so a bundle that stays stale cannot loop. */
const RELOADED_KEY = "puddle.reloaded-for-build";

export interface BuildGuardDeps {
  /** The build this page runs. */
  current: string;
  source?: LiveSource;
  api?: Pick<ApiClient, "GET">;
  /** The page's location, whose `reload` loads the page again; the window's own by default. */
  location?: Pick<Location, "reload">;
  /** Where the last build reloaded for is remembered; the tab's session storage by default. */
  memory?: Pick<Storage, "getItem" | "setItem">;
  pollMs?: number;
}

function sessionMemory(): Pick<Storage, "getItem" | "setItem"> | undefined {
  try {
    return window.sessionStorage;
  } catch {
    return undefined;
  }
}

export class BuildGuard {
  readonly #current: string;
  readonly #source: LiveSource | undefined;
  readonly #api: Pick<ApiClient, "GET">;
  readonly #location: Pick<Location, "reload">;
  readonly #memory: Pick<Storage, "getItem" | "setItem"> | undefined;
  readonly #pollMs: number;

  constructor(deps: BuildGuardDeps) {
    this.#current = deps.current;
    this.#source = deps.source;
    this.#api = deps.api ?? defaultApi;
    this.#location = deps.location ?? window.location;
    this.#memory = "memory" in deps ? deps.memory : sessionMemory();
    this.#pollMs = deps.pollMs ?? 60_000;
  }

  /**
   * Reads the server's build and reloads when it is another one than this page's. Never throws:
   * a server that does not answer has nothing to compare with. Returns whether it reloaded.
   */
  async check(): Promise<boolean> {
    let served: string | null | undefined;
    try {
      const { data } = await this.#api.GET("/api/health");
      served = data?.ui_build_id;
    } catch {
      return false;
    }
    if (!served || served === this.#current) return false;
    if (this.#remembered() === served) return false;
    this.#remember(served);
    this.#location.reload();
    return true;
  }

  /** Starts watching; returns the function that stops it. */
  start(): () => void {
    void this.check();
    const stopListening = this.#source?.subscribe({
      resync: () => void this.check(),
    });
    const timer = setInterval(() => void this.check(), this.#pollMs);
    return () => {
      stopListening?.();
      clearInterval(timer);
    };
  }

  #remembered(): string | null {
    try {
      return this.#memory?.getItem(RELOADED_KEY) ?? null;
    } catch {
      return null;
    }
  }

  #remember(build: string): void {
    try {
      this.#memory?.setItem(RELOADED_KEY, build);
    } catch {
      // Without storage a stale bundle may reload again at the next check; the page still works.
    }
  }
}
