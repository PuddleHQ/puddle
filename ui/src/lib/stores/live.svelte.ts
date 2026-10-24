// SPDX-License-Identifier: GPL-3.0-or-later
/* eslint-disable svelte/prefer-svelte-reactivity -- the maps and sets here are bookkeeping that no template reads, so they need no reactivity */
// Shell-level live state: the pending-request count for the nav badge and the document title,
// and how the event stream is doing. Screens that need more subscribe to `eventStream` themselves.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import {
  eventStream,
  type EventStreamOptions,
  type StreamState,
} from "#lib/api/sse.ts";
import { readFailure } from "./read-failure.ts";

/** Why the count could not be read. */
export type LiveProblem = "unauthorized" | "unreachable" | "failing";

/** What a screen hears from the shell's one event stream. */
export interface LiveListener {
  event?: (event: unknown) => void;
  /** Events were or may have been missed (`lagged`, reconnect): refetch what the screen shows. */
  resync?: () => void;
}

/** Where a screen subscribes to the shell's stream instead of opening a second connection. */
export interface LiveSource {
  subscribe(listener: LiveListener): () => void;
}

export interface LiveDeps {
  api?: Pick<ApiClient, "GET">;
  events?: (options: EventStreamOptions) => AsyncIterable<unknown>;
  /** Interim refresh while the API has no pending-request events. */
  pollMs?: number;
}

export class LiveState implements LiveSource {
  pending = $state(0);
  stream = $state<StreamState>("connecting");
  problem = $state<LiveProblem | null>(null);
  /** For `failing`: what the service answered. */
  problemDetail = $state<string | null>(null);
  /** Why the event stream last failed to connect; `null` while it is connected. */
  streamError = $state<string | null>(null);

  readonly #api: Pick<ApiClient, "GET">;
  readonly #events: (options: EventStreamOptions) => AsyncIterable<unknown>;
  readonly #pollMs: number;
  readonly #listeners = new Set<LiveListener>();
  /** Opens the stream again; set while `start` runs. */
  #reopen: (() => void) | null = null;

  constructor(deps: LiveDeps = {}) {
    this.#api = deps.api ?? defaultApi;
    this.#events = deps.events ?? eventStream;
    this.#pollMs = deps.pollMs ?? 15_000;
  }

  /** Screens that show more than the count listen here; returns the function that stops it. */
  subscribe(listener: LiveListener): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  /** A screen that knows the count (the inbox, after a decision) tells the badge at once. */
  setPending(count: number): void {
    this.pending = count;
  }

  /** Reads the open-request count. Never throws: a failure sets `problem`. */
  async refresh(): Promise<void> {
    try {
      const { data, error, response } = await this.#api.GET("/api/pending");
      if (data) {
        this.pending = data.requests.length;
        this.problem = null;
        this.problemDetail = null;
        // The service answers this app again after refusing its key on the stream (a restart, a
        // key handover): the stream was stopped for good, so it is opened anew.
        if (this.stream === "refused") this.#reopen?.();
      } else if (response.status === 401) {
        this.problem = "unauthorized";
      } else {
        this.problem = "failing";
        this.problemDetail = readFailure(error, response);
      }
    } catch {
      this.problem = "unreachable";
    }
  }

  /** Starts the stream and the poll; returns the function that stops both. */
  start(): () => void {
    const controller = new AbortController();
    let streaming: AbortController | null = null;
    const open = () => {
      streaming?.abort();
      const own = new AbortController();
      streaming = own;
      controller.signal.addEventListener("abort", () => own.abort(), {
        once: true,
      });
      this.stream = "connecting";
      void (async () => {
        const stream = this.#events({
          signal: own.signal,
          onState: (state) => {
            if (own.signal.aborted) return;
            this.stream = state;
            if (state === "connected") this.streamError = null;
          },
          onError: (reason) => {
            if (!own.signal.aborted) this.streamError = reason;
          },
          onResync: () => {
            void this.refresh();
            for (const l of [...this.#listeners]) l.resync?.();
          },
        });
        for await (const event of stream) {
          // The count itself still comes from the poll until the API sends pending events.
          for (const l of [...this.#listeners]) l.event?.(event);
        }
      })();
    };
    this.#reopen = open;
    void this.refresh();
    const timer = setInterval(() => void this.refresh(), this.#pollMs);
    open();
    return () => {
      controller.abort();
      clearInterval(timer);
      this.#reopen = null;
    };
  }
}

export const live = new LiveState();
