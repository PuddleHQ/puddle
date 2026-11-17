// SPDX-License-Identifier: AGPL-3.0-or-later
// The id a page gives itself when it tells the service about itself (`POST /api/presence`). It
// lives as long as the tab does: a reload keeps it, a second tab has its own. It names no
// person and no device; it only lets the service tell the tabs apart.
//
// A duplicated tab copies the first one's session storage, id included. Each open page therefore
// marks its id in local storage (shared by all tabs) with a token of its own and a time, and a
// page that finds a recent mark by another page on the id it was given makes a new one.

const KEY = "puddle.client";
const MARK = "puddle.client.live.";
const VALID = /^[A-Za-z0-9_-]{1,64}$/;
/** How often a page renews its mark. */
const RENEW_MS = 20_000;
/**
 * How long a mark counts. Longer than a background tab's slowest timer (a minute), so a hidden
 * original is still seen; a page that died without clearing its mark only costs a new id.
 */
const FRESH_MS = 90_000;
/** What tells this page's mark from another page's. */
const PAGE = fresh();

type Marks = Pick<Storage, "getItem" | "setItem" | "removeItem">;

function takenByAnother(
  marks: Marks | undefined,
  id: string,
  now: number,
): boolean {
  try {
    const [token, at] = (marks?.getItem(MARK + id) ?? "").split(":");
    return (
      token !== undefined &&
      token !== "" &&
      token !== PAGE &&
      now - Number(at) < FRESH_MS
    );
  } catch {
    return false;
  }
}

function fresh(): string {
  const crypto = globalThis.crypto;
  if (typeof crypto?.randomUUID === "function") return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** This tab's client id, made on first use and kept in the tab's session storage. */
export function clientId(
  storage: Pick<Storage, "getItem" | "setItem"> | undefined = safeStorage(),
  marks: Marks | undefined = safeMarks(),
  now: () => number = Date.now,
): string {
  try {
    const kept = storage?.getItem(KEY);
    if (kept && VALID.test(kept) && !takenByAnother(marks, kept, now())) {
      return kept;
    }
  } catch {
    // Storage that cannot be read: the id lasts for this page load.
  }
  const made = fresh();
  try {
    storage?.setItem(KEY, made);
  } catch {
    // Not kept; the next load makes another.
  }
  return made;
}

function safeStorage(): Storage | undefined {
  try {
    return globalThis.sessionStorage;
  } catch {
    return undefined;
  }
}

function safeMarks(): Storage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

/** Marks `id` as this open page's, and keeps marking it until the page goes; returns the stop. */
export function keepClientId(
  id: string,
  marks: Marks | undefined = safeMarks(),
  now: () => number = Date.now,
): () => void {
  const mark = () => {
    try {
      marks?.setItem(MARK + id, `${PAGE}:${now()}`);
    } catch {
      // No local storage: a duplicate of this tab keeps this id.
    }
  };
  const clear = () => {
    try {
      if (!takenByAnother(marks, id, now())) marks?.removeItem(MARK + id);
    } catch {
      // Nothing to clear.
    }
  };
  mark();
  const timer = setInterval(mark, RENEW_MS);
  addEventListener("pagehide", clear);
  return () => {
    clearInterval(timer);
    removeEventListener("pagehide", clear);
    clear();
  };
}
