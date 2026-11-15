// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  activeToken,
  authHeaders,
  bootstrapToken,
  useSessionToken,
} from "./connection.ts";

describe("bootstrapToken", () => {
  it("reads the token the shell set on the page", () => {
    expect(bootstrapToken({ __PUDDLE__: { token: "abc" } })).toBe("abc");
  });

  it("is undefined without a bootstrap object, a token, or with an empty or wrong-typed one", () => {
    expect(bootstrapToken({})).toBeUndefined();
    expect(bootstrapToken({ __PUDDLE__: {} })).toBeUndefined();
    expect(bootstrapToken({ __PUDDLE__: { token: "" } })).toBeUndefined();
    expect(
      bootstrapToken({ __PUDDLE__: { token: 5 as unknown as string } }),
    ).toBeUndefined();
  });

  it("looks at the global scope by default", () => {
    expect(bootstrapToken()).toBeUndefined();
    (globalThis as { __PUDDLE__?: { token: string } }).__PUDDLE__ = {
      token: "g",
    };
    try {
      expect(bootstrapToken()).toBe("g");
    } finally {
      delete (globalThis as { __PUDDLE__?: unknown }).__PUDDLE__;
    }
  });

  it("never reads the URL, cookies or storage", () => {
    window.history.replaceState(null, "", "/?token=from-url#token=from-hash");
    document.cookie = "token=from-cookie";
    localStorage.setItem("token", "from-storage");
    expect(bootstrapToken()).toBeUndefined();
    window.history.replaceState(null, "", "/");
    localStorage.clear();
  });
});

describe("authHeaders", () => {
  it("is a bearer header with a token and empty without", () => {
    expect(authHeaders("t")).toEqual({ Authorization: "Bearer t" });
    expect(authHeaders(undefined)).toEqual({});
  });
});

describe("activeToken", () => {
  it("is the shell's token first, then this browser's, then none", () => {
    useSessionToken(() => undefined);
    expect(activeToken()).toBeUndefined();
    useSessionToken(() => "session");
    expect(activeToken()).toBe("session");
    (globalThis as { __PUDDLE__?: { token: string } }).__PUDDLE__ = {
      token: "shell",
    };
    try {
      expect(activeToken()).toBe("shell");
    } finally {
      delete (globalThis as { __PUDDLE__?: unknown }).__PUDDLE__;
      useSessionToken(() => undefined);
    }
  });
});
