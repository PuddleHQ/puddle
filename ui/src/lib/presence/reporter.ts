// SPDX-License-Identifier: AGPL-3.0-or-later
// Tells the service whether this page is in front and when the person last used it, so that a
// new request is announced on the one place the person is, and not on every tab, phone and window
// they have open (`POST /api/presence`). It reports every 30 seconds and whenever one of the
// facts changes. A report that fails is ignored: the next one replaces it.
import type { ApiClient } from "#lib/api/client.ts";

export type ClientKind = "browser" | "desktop";

/** What the reporter listens to: the document and the window. */
export interface PresenceTarget {
  visibilityState: DocumentVisibilityState;
  addEventListener(type: string, listener: () => void, options?: object): void;
  removeEventListener(type: string, listener: () => void): void;
}

export interface ReporterDeps {
  api: Pick<ApiClient, "POST">;
  clientId: string;
  kind: ClientKind;
  /** Whether the person wants notifications on this client right now. */
  notify: () => boolean;
  document?: PresenceTarget;
  window?: Pick<PresenceTarget, "addEventListener" | "removeEventListener">;
  now?: () => number;
  /** How often to report while nothing changes. */
  everyMs?: number;
}

/** How often a client reports; the service counts a client silent for 75 s as gone. */
export const REPORT_EVERY_MS = 30_000;

const INPUT = ["pointerdown", "pointermove", "keydown", "wheel", "touchstart"];
const CHANGE = ["visibilitychange"];
const WINDOW_CHANGE = ["focus", "blur", "pageshow"];
/** What a page that is going away reports as its idle time: long past anything that counts as in use. */
const LEFT_IDLE_MS = 3_600_000;

export class PresenceReporter {
  readonly #deps: ReporterDeps;
  readonly #now: () => number;
  #lastInput: number;
  #sent: { visible: boolean; notify: boolean } | null = null;

  constructor(deps: ReporterDeps) {
    this.#deps = deps;
    this.#now = deps.now ?? Date.now;
    this.#lastInput = this.#now();
  }

  /** Reports now, then keeps reporting; returns the function that stops it. */
  start(): () => void {
    const doc = this.#deps.document ?? document;
    const win = this.#deps.window ?? window;
    const onInput = () => {
      this.#lastInput = this.#now();
    };
    const onChange = () => this.refresh();
    for (const type of INPUT)
      doc.addEventListener(type, onInput, { passive: true });
    for (const type of CHANGE) doc.addEventListener(type, onChange);
    for (const type of WINDOW_CHANGE) win.addEventListener(type, onChange);
    const onLeave = () => this.#leave();
    win.addEventListener("pagehide", onLeave);
    const timer = setInterval(
      () => void this.report(),
      this.#deps.everyMs ?? REPORT_EVERY_MS,
    );
    void this.report();
    return () => {
      clearInterval(timer);
      win.removeEventListener("pagehide", onLeave);
      for (const type of INPUT) doc.removeEventListener(type, onInput);
      for (const type of CHANGE) doc.removeEventListener(type, onChange);
      for (const type of WINDOW_CHANGE) win.removeEventListener(type, onChange);
    };
  }

  /** Something the report says may have changed (visibility, the switch): reports if it did. */
  refresh(): void {
    const now = this.#facts();
    if (
      this.#sent === null ||
      this.#sent.visible !== now.visible ||
      this.#sent.notify !== now.notify
    ) {
      void this.report();
    }
  }

  #facts(): { visible: boolean; notify: boolean } {
    const doc = this.#deps.document ?? document;
    return {
      visible: doc.visibilityState === "visible",
      notify: this.#deps.notify(),
    };
  }

  /** Sends one report. Never throws. */
  async report(): Promise<void> {
    const facts = this.#facts();
    this.#sent = facts;
    // A page being looked at is in use at this moment, whatever the last input event was.
    const idle = Math.max(0, this.#now() - this.#lastInput);
    await this.#send({
      visible: facts.visible,
      idle_ms: Math.round(idle),
      notify: facts.notify,
    });
  }

  /** The page is going away: say so, so the service stops picking it before its report expires. */
  #leave(): void {
    this.#sent = { visible: false, notify: false };
    void this.#send(
      { visible: false, idle_ms: LEFT_IDLE_MS, notify: false },
      true,
    );
  }

  async #send(
    facts: { visible: boolean; idle_ms: number; notify: boolean },
    keepalive = false,
  ): Promise<void> {
    try {
      await this.#deps.api.POST("/api/presence", {
        body: { client: this.#deps.clientId, kind: this.#deps.kind, ...facts },
        ...(keepalive ? { keepalive: true } : {}),
      });
    } catch {
      // The next report replaces it.
    }
  }
}
