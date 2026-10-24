// SPDX-License-Identifier: GPL-3.0-or-later
// The event stream reader (ADR 0004). `EventSource` can't send the bearer token, so this reads
// `GET /api/events` with `fetch` and feeds the bytes to eventsource-parser. It is an async
// iterator of `Event`: it reconnects with backoff, tells the caller when a refetch is needed
// (`lagged`, or any reconnect, because events may have been missed), and ends when aborted or
// when the service refuses the app's key (401/403: the same key will be refused again).
import { createParser } from "eventsource-parser";
import { authHeaders, bootstrapToken } from "./connection.ts";
import type { components } from "./schema.d.ts";

export type PuddleEvent = components["schemas"]["Event"];
export type Lagged = components["schemas"]["Lagged"];

export type StreamState =
  | "connecting"
  | "connected"
  | "reconnecting"
  /** The service refused the app's key; the stream has stopped and will not retry. */
  | "refused";

/** Why the service refused the stream (401 or 403) and what the user can do about it. */
export interface StreamRefusal {
  reason: string;
  advice: string;
}

const ADVICE_OTHER =
  "Open Puddle from its app window or tray icon; if this stays, restart the app.";

/**
 * Reads the service's own error body (`{ error, message }`) for a 401 or 403. The code
 * picks the advice; the message is shown as the service wrote it, cut to a sensible length.
 */
export async function refusalOf(response: Response): Promise<StreamRefusal> {
  let code: unknown;
  let message = "";
  try {
    const body: unknown = JSON.parse(await response.text());
    if (typeof body === "object" && body !== null) {
      code = (body as { error?: unknown }).error;
      const text = (body as { message?: unknown }).message;
      if (typeof text === "string") {
        message = text.trim().replace(/\.$/, "").slice(0, 200);
      }
    }
  } catch {
    // No readable body (a proxy, a stub): the status alone is the reason.
  }
  const reason =
    message === ""
      ? `the service refused the event stream (status ${response.status})`
      : `the service refused the event stream: ${message} (status ${response.status})`;
  const advice =
    code === "forbidden_origin"
      ? "Open Puddle from its own app window or tray icon, not from another address."
      : code === "unauthorized"
        ? "Restart Puddle's app to sign in again."
        : ADVICE_OTHER;
  return { reason, advice };
}

export interface EventStreamOptions {
  /** Ends the stream; the iterator then returns without throwing. */
  signal: AbortSignal;
  /** Only this workspace's events (global ones always come through). */
  workspace?: string;
  /** Looked up per connection attempt. */
  getToken?: () => string | undefined;
  /** Called when events were or may have been missed: refetch what the screen shows. */
  onResync?: (reason: "lagged" | "reconnect", lagged?: Lagged) => void;
  /** Called on every connection state change, for a "reconnecting" hint. */
  onState?: (state: StreamState) => void;
  /** Called when an attempt failed, with why in words ("the event stream answered 500"); not called after a clean close or an abort. */
  onError?: (reason: string) => void;
  /** Called once when the service refused the stream (401/403), just before it stops. */
  onRefused?: (refusal: StreamRefusal) => void;
  /** Wait times between attempts; the last repeats. */
  backoffMs?: readonly number[];
  /** For tests. */
  fetch?: typeof fetch;
  /** For tests: resolves after `ms` or when the signal aborts. */
  sleep?: (ms: number, signal: AbortSignal) => Promise<void>;
  /** The stream's URL; for tests. */
  url?: string;
}

export const DEFAULT_BACKOFF_MS: readonly number[] = [1000, 2000, 5000, 10_000];

export function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const done = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done);
  });
}

function parseEvent(data: string): PuddleEvent | undefined {
  try {
    const value: unknown = JSON.parse(data);
    if (typeof value === "object" && value !== null && "type" in value) {
      return value as PuddleEvent;
    }
  } catch {
    // A frame that isn't JSON is skipped; the stream carries on.
  }
  return undefined;
}

function streamUrl(options: EventStreamOptions): string {
  const base = options.url ?? "/api/events";
  return options.workspace === undefined
    ? base
    : `${base}?workspace=${encodeURIComponent(options.workspace)}`;
}

/**
 * Yields events until `signal` aborts. Never throws for network or HTTP errors: it retries,
 * except after a 401 or 403, where it reports why and ends (the key won't change by waiting).
 */
export async function* eventStream(
  options: EventStreamOptions,
): AsyncGenerator<PuddleEvent, void> {
  const doFetch =
    options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const wait = options.sleep ?? sleep;
  const backoff = options.backoffMs?.length
    ? options.backoffMs
    : DEFAULT_BACKOFF_MS;
  const { signal } = options;
  let failures = 0;
  let everConnected = false;
  options.onState?.("connecting");

  while (!signal.aborted) {
    let failure = "the connection to the event stream failed";
    let refused: StreamRefusal | null = null;
    try {
      const response = await doFetch(streamUrl(options), {
        signal,
        headers: {
          Accept: "text/event-stream",
          ...authHeaders((options.getToken ?? bootstrapToken)()),
        },
      });
      if (!response.ok || response.body === null) {
        if (response.status === 401 || response.status === 403) {
          refused = await refusalOf(response);
          failure = refused.reason;
        } else {
          failure = `the event stream answered ${response.status}`;
        }
        throw new Error(`event stream answered ${response.status}`);
      }
      if (everConnected) options.onResync?.("reconnect");
      everConnected = true;
      failures = 0;
      options.onState?.("connected");

      const queue: PuddleEvent[] = [];
      const parser = createParser({
        onEvent(message) {
          if (message.event === "lagged") {
            let lagged: Lagged | undefined;
            try {
              lagged = JSON.parse(message.data) as Lagged;
            } catch {
              lagged = undefined;
            }
            options.onResync?.("lagged", lagged);
            return;
          }
          const event = parseEvent(message.data);
          if (event) queue.push(event);
        },
      });
      const reader = response.body
        .pipeThrough(new TextDecoderStream())
        .getReader();
      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          parser.feed(value);
          while (queue.length > 0) yield queue.shift() as PuddleEvent;
        }
      } finally {
        reader.cancel().catch(() => undefined);
      }
    } catch {
      if (signal.aborted) break;
      options.onError?.(failure);
    }
    if (signal.aborted) break;
    if (refused !== null) {
      options.onRefused?.(refused);
      options.onState?.("refused");
      return;
    }
    options.onState?.("reconnecting");
    const delay = backoff[Math.min(failures, backoff.length - 1)] ?? 1000;
    failures += 1;
    await wait(delay, signal);
  }
}
