// SPDX-License-Identifier: AGPL-3.0-or-later
// Sends the errors no code expected to the server, which scrubs them and writes them to its log,
// so a page that broke leaves a trace beyond the browser's console. The server bounds and
// rate-limits what it takes; this side keeps a page in a loop of errors from sending the same one
// over and over, and never throws or reports its own failure.
import type { components } from "#lib/api/schema.d.ts";
import { errorParts } from "./unexpected.ts";

export type UiErrorKind = components["schemas"]["UiErrorKind"];
type UiErrorReport = components["schemas"]["UiErrorReport"];

/** The most reports one page load sends; a page past it has said enough. */
export const MAX_PER_PAGE = 20;
/** How long to hold back after the server said "too many" without saying how long. */
export const DEFAULT_PAUSE_MS = 60_000;

export interface ReporterDeps {
  /** Sends one report; resolves to the answer's status and `Retry-After` seconds, if any. */
  send: (
    report: UiErrorReport,
  ) => Promise<{ status: number; retryAfterS?: number }>;
  /** The page's path now. */
  route: () => string;
  /** The build of the app this page runs. */
  build: string;
  now?: () => number;
}

export class ErrorReporter {
  readonly #deps: ReporterDeps;
  readonly #now: () => number;
  readonly #sent = new Set<string>();
  #pausedUntil = 0;

  constructor(deps: ReporterDeps) {
    this.#deps = deps;
    this.#now = deps.now ?? Date.now;
  }

  /** Reports `error`, unless this page already sent it, sent its fill, or the server said to wait. */
  report(kind: UiErrorKind, error: unknown): void {
    const { message, stack } = errorParts(error);
    const key = `${kind}\n${message}`;
    if (this.#sent.has(key) || this.#sent.size >= MAX_PER_PAGE) return;
    if (this.#now() < this.#pausedUntil) return;
    this.#sent.add(key);
    void this.#send({
      kind,
      message,
      stack,
      route: this.#deps.route(),
      build: this.#deps.build,
    });
  }

  async #send(report: UiErrorReport): Promise<void> {
    try {
      const answer = await this.#deps.send(report);
      if (answer.status === 429) {
        const pause =
          answer.retryAfterS !== undefined
            ? answer.retryAfterS * 1000
            : DEFAULT_PAUSE_MS;
        this.#pausedUntil = this.#now() + pause;
      }
    } catch {
      // A server that does not answer cannot log it either; the console already has it.
    }
  }
}

/** The page events of errors nothing caught. */
export interface ErrorEvents {
  addEventListener(type: "error", listener: (event: ErrorEvent) => void): void;
  addEventListener(
    type: "unhandledrejection",
    listener: (event: PromiseRejectionEvent) => void,
  ): void;
}

/** Reports every error `target` (the window) sees that nothing caught. */
export function reportUncaught(
  target: ErrorEvents,
  reporter: ErrorReporter,
): void {
  target.addEventListener("error", (event) => {
    // An `error` with neither a value nor a message says nothing worth sending (a resource that
    // failed to load is such an event, and the console names it).
    const value: unknown = event.error;
    if ((value === undefined || value === null) && !event.message) return;
    reporter.report("uncaught", value ?? event.message);
  });
  target.addEventListener("unhandledrejection", (event) => {
    reporter.report("unhandled_rejection", event.reason);
  });
}
