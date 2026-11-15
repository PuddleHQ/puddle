// SPDX-License-Identifier: AGPL-3.0-or-later
// The terminal socket's vocabulary: the sub-protocol, the size limits, the control
// messages the page sends as text and the one the host sends back. Pure, so every rule is tested
// without a socket.

/** The first `Sec-WebSocket-Protocol` entry; the ticket follows as `ticket.<hex>`. */
export const SUBPROTOCOL = "puddle-terminal.v1";

/** One input message holds at most this many bytes; a longer paste is split. */
export const MAX_INPUT_BYTES = 64 * 1024;

export const MIN_COLS = 2;
export const MAX_COLS = 500;
export const MIN_ROWS = 1;
export const MAX_ROWS = 300;

/** At most this many terminals per workspace and in all. */
export const MAX_PER_WORKSPACE = 8;
export const MAX_IN_ALL = 32;

export type EndReason =
  "exited" | "stopped" | "crashed" | "boot_failed" | "quitting";

/** What the host says when a terminal is over. */
export interface Ended {
  reason: EndReason;
  /** The shell's exit status, for `exited` when it gave one. */
  status: number | null;
  /** Text for the user; it can quote the guest, so it is drawn as text only. */
  message: string | null;
}

export interface Size {
  cols: number;
  rows: number;
}

/** `size` held to the limits the host enforces; a fractional or missing size becomes the nearest whole one. */
export function clampSize(cols: number, rows: number): Size {
  const whole = (n: number, min: number, max: number) =>
    Math.min(max, Math.max(min, Number.isFinite(n) ? Math.floor(n) : min));
  return {
    cols: whole(cols, MIN_COLS, MAX_COLS),
    rows: whole(rows, MIN_ROWS, MAX_ROWS),
  };
}

export const resizeMessage = (size: Size): string =>
  JSON.stringify({ type: "resize", cols: size.cols, rows: size.rows });

export const ackMessage = (bytes: number): string =>
  JSON.stringify({ type: "ack", bytes });

const REASONS: readonly string[] = [
  "exited",
  "stopped",
  "crashed",
  "boot_failed",
  "quitting",
];

/** The host's `ended` message, or `null` for any other text. */
export function parseEnded(text: string): Ended | null {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const { type, reason, status, message } = value as Record<string, unknown>;
  if (type !== "ended" || typeof reason !== "string") return null;
  if (!REASONS.includes(reason)) return null;
  return {
    reason: reason as EndReason,
    status: typeof status === "number" ? status : null,
    message: typeof message === "string" ? message : null,
  };
}

/** `bytes` in pieces of at most `MAX_INPUT_BYTES`, in order. */
export function splitInput(bytes: Uint8Array): Uint8Array[] {
  const pieces: Uint8Array[] = [];
  for (let at = 0; at < bytes.length; at += MAX_INPUT_BYTES) {
    pieces.push(bytes.subarray(at, at + MAX_INPUT_BYTES));
  }
  return pieces;
}

/** The socket's address for a terminal on the page's own origin, `ws` or `wss` after the page. */
export function socketUrl(
  origin: string,
  workspace: string,
  terminal: number,
): string {
  const base = new URL(origin);
  base.protocol = base.protocol === "https:" ? "wss:" : "ws:";
  base.pathname = `/api/workspaces/${encodeURIComponent(workspace)}/terminals/${terminal}`;
  return base.toString();
}
