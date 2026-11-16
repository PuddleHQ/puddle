// SPDX-License-Identifier: AGPL-3.0-or-later
// The id a page gives itself when it tells the service about itself (`POST /api/presence`). It
// lives as long as the tab does: a reload keeps it, a second tab has its own. It names no
// person and no device; it only lets the service tell the tabs apart.

const KEY = "puddle.client";
const VALID = /^[A-Za-z0-9_-]{1,64}$/;

function fresh(): string {
  const crypto = globalThis.crypto;
  if (typeof crypto?.randomUUID === "function") return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** This tab's client id, made on first use and kept in the tab's session storage. */
export function clientId(
  storage: Pick<Storage, "getItem" | "setItem"> | undefined = safeStorage(),
): string {
  try {
    const kept = storage?.getItem(KEY);
    if (kept && VALID.test(kept)) return kept;
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
