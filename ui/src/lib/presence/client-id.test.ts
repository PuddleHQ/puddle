// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import { clientId } from "./client-id.ts";

const store = (initial: Record<string, string> = {}) => {
  const data = new Map(Object.entries(initial));
  return {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
    data,
  };
};

describe("clientId", () => {
  it("makes an id the service accepts and keeps it for the tab", () => {
    const s = store();
    const id = clientId(s);
    expect(id).toMatch(/^[A-Za-z0-9_-]{1,64}$/);
    expect(clientId(s)).toBe(id);
    expect(s.data.get("puddle.client")).toBe(id);
  });

  it("makes another when the kept one is not a valid id", () => {
    const s = store({ "puddle.client": "has space" });
    expect(clientId(s)).not.toBe("has space");
  });

  it("still gives an id when storage cannot be read or written", () => {
    const broken = {
      getItem: () => {
        throw new Error("no");
      },
      setItem: () => {
        throw new Error("no");
      },
    };
    expect(clientId(broken)).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(clientId(undefined)).toMatch(/^[A-Za-z0-9_-]+$/);
  });

  it("falls back to random bytes where randomUUID is missing", () => {
    vi.stubGlobal("crypto", {
      getRandomValues: (a: Uint8Array) => a.fill(10),
    });
    try {
      expect(clientId(store())).toBe("0a".repeat(16));
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("uses the tab's session storage by default", () => {
    sessionStorage.clear();
    const id = clientId();
    expect(sessionStorage.getItem("puddle.client")).toBe(id);
  });

  it("works where reading session storage throws", () => {
    vi.spyOn(globalThis, "sessionStorage", "get").mockImplementation(() => {
      throw new Error("blocked");
    });
    try {
      expect(clientId()).toMatch(/^[A-Za-z0-9_-]+$/);
    } finally {
      vi.restoreAllMocks();
    }
  });
});
