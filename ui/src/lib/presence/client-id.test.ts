// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import { clientId, keepClientId } from "./client-id.ts";

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

describe("a duplicated tab", () => {
  const marks = () => {
    const data = new Map<string, string>();
    return {
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => void data.set(k, v),
      removeItem: (k: string) => void data.delete(k),
      data,
    };
  };
  /** Another page's mark: not this page's token. */
  const otherPage = (m: ReturnType<typeof marks>, id: string, at: number) =>
    m.data.set(`puddle.client.live.${id}`, `other-page:${at}`);

  it("keeps its id while no other page has marked it", () => {
    const s = store({ "puddle.client": "tab-1" });
    expect(clientId(s, marks(), () => 1000)).toBe("tab-1");
  });

  it("gets a new id when another open page has marked the copied one", () => {
    const s = store({ "puddle.client": "tab-1" });
    const m = marks();
    otherPage(m, "tab-1", 1000);
    const id = clientId(s, m, () => 5000);
    expect(id).not.toBe("tab-1");
    expect(s.data.get("puddle.client")).toBe(id);
  });

  it("keeps its id after the other page's mark has gone stale", () => {
    const s = store({ "puddle.client": "tab-1" });
    const m = marks();
    otherPage(m, "tab-1", 1000);
    expect(clientId(s, m, () => 1000 + 91_000)).toBe("tab-1");
  });

  it("does not take its own mark for another page's, across a reload", () => {
    const s = store({ "puddle.client": "tab-1" });
    const m = marks();
    const stop = keepClientId("tab-1", m, () => 1000);
    expect(m.data.get("puddle.client.live.tab-1")).toMatch(/:1000$/);
    expect(clientId(s, m, () => 2000)).toBe("tab-1");
    stop();
    expect(m.data.has("puddle.client.live.tab-1")).toBe(false);
  });

  it("renews its mark while the page is open and leaves another page's mark alone", () => {
    vi.useFakeTimers();
    try {
      const m = marks();
      let clock = 1000;
      const stop = keepClientId("tab-1", m, () => clock);
      clock = 21_000;
      vi.advanceTimersByTime(20_000);
      expect(m.data.get("puddle.client.live.tab-1")).toMatch(/:21000$/);
      otherPage(m, "tab-1", clock);
      stop();
      expect(m.data.get("puddle.client.live.tab-1")).toBe("other-page:21000");
    } finally {
      vi.useRealTimers();
    }
  });

  it("works where local storage cannot be used", () => {
    const broken = {
      getItem: () => {
        throw new Error("no");
      },
      setItem: () => {
        throw new Error("no");
      },
      removeItem: () => {
        throw new Error("no");
      },
    };
    expect(clientId(store({ "puddle.client": "tab-1" }), broken)).toBe("tab-1");
    keepClientId("tab-1", broken)();
    keepClientId("tab-1", undefined)();
  });

  it("works where reading local storage throws", () => {
    vi.spyOn(globalThis, "localStorage", "get").mockImplementation(() => {
      throw new Error("blocked");
    });
    try {
      expect(clientId(store({ "puddle.client": "tab-1" }))).toBe("tab-1");
      keepClientId("tab-1")();
    } finally {
      vi.restoreAllMocks();
    }
  });

  it("clears its mark when the page is hidden for good", () => {
    const m = marks();
    const stop = keepClientId("tab-1", m);
    dispatchEvent(new Event("pagehide"));
    expect(m.data.has("puddle.client.live.tab-1")).toBe(false);
    stop();
  });
});
