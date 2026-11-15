// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";
import { activeToken } from "#lib/api/connection.ts";
import {
  REFRESH_EARLY_MS,
  RETRY_MS,
  SessionStore,
  type SessionGrant,
} from "./session.svelte.ts";

type Answer =
  | { data: unknown; response: { status: number; ok?: boolean } }
  | { error: unknown; response: { status: number; ok?: boolean } }
  | "throw";

const NOW = 1_000_000;
const grant = (
  token: string,
  over: Partial<SessionGrant> = {},
): SessionGrant => ({
  access_token: token,
  expires_at: NOW + 15 * 60_000,
  user: { id: 1, name: "Tijs", role: "owner" },
  device: {
    id: 7,
    name: "Firefox on Linux",
    created_at: NOW,
    last_seen_at: NOW,
    expires_at: NOW + 1,
    current: true,
  },
  ...over,
});
const ok = (data: unknown, status = 200): Answer => ({
  data,
  response: { status, ok: true },
});
const refused = (status: number, message: string): Answer => ({
  error: { error: "x", message },
  response: { status, ok: false },
});

let answers: Record<string, Answer[]>;
let calls: { method: string; path: string; body: unknown }[];
let timers: {
  run: () => void;
  ms: number;
  cleared: boolean;
  fired: boolean;
}[];
let dropped: number;

function answer(method: string, path: string, init?: { body?: unknown }) {
  calls.push({ method, path, body: init?.body });
  const next = answers[`${method} ${path}`]?.shift();
  if (next === "throw") throw new Error("down");
  return Promise.resolve(next ?? refused(404, "no such route"));
}
const api = {
  GET: (path: string) => answer("GET", path),
  POST: (path: string, init?: { body?: unknown }) => answer("POST", path, init),
};

function make(over: Record<string, unknown> = {}) {
  return new SessionStore({
    api: api as never,
    bootstrap: () => undefined,
    now: () => NOW,
    setTimer: (run, ms) => {
      const timer = {
        run: () => {
          timer.fired = true;
          run();
        },
        ms,
        cleared: false,
        fired: false,
      };
      timers.push(timer);
      return timer;
    },
    clearTimer: (timer) => {
      (timer as { cleared: boolean }).cleared = true;
    },
    userAgent: () =>
      "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0",
    location: () => ({ pathname: "/inbox", hash: "" }),
    dropFragment: () => {
      dropped += 1;
    },
    ...over,
  });
}

const status = (setup_open: boolean, password_login = true) =>
  ok({ setup_open, password_login });

beforeEach(() => {
  answers = {};
  calls = [];
  timers = [];
  dropped = 0;
});

describe("start", () => {
  it("asks nothing when the desktop shell gave the page its token", async () => {
    const session = make({ bootstrap: () => "shell" });
    await session.start();
    expect(session.mode).toBe("local");
    expect(session.ready).toBe(true);
    expect(calls).toEqual([]);
  });

  it("treats an address with no sign-in as local (a development proxy adds the token)", async () => {
    answers["GET /api/auth/status"] = [refused(404, "no such route")];
    const session = make();
    await session.start();
    expect(session.mode).toBe("local");
  });

  it("says unavailable when the service has no accounts or does not answer", async () => {
    answers["GET /api/auth/status"] = [refused(503, "not available"), "throw"];
    const session = make();
    await session.start();
    expect(session.mode).toBe("unavailable");
    await session.start();
    expect(session.mode).toBe("unavailable");
    expect(session.ready).toBe(false);
  });

  it("goes to setup for the first visitor without trying to refresh", async () => {
    answers["GET /api/auth/status"] = [status(true)];
    const session = make();
    await session.start();
    expect(session.mode).toBe("setup");
    expect(calls.map((c) => c.path)).toEqual(["/api/auth/status"]);
  });

  it("signs in again from the cookie and schedules the next refresh a minute early", async () => {
    answers["GET /api/auth/status"] = [status(false)];
    answers["POST /api/auth/refresh"] = [ok(grant("first"))];
    const session = make();
    await session.start();
    expect(session.mode).toBe("signed-in");
    expect(session.token()).toBe("first");
    expect(activeToken()).toBe("first");
    expect(session.user?.name).toBe("Tijs");
    expect(session.device?.current).toBe(true);
    expect(timers).toHaveLength(1);
    expect(timers[0]?.ms).toBe(15 * 60_000 - REFRESH_EARLY_MS);
  });

  it("asks for a name and a password when the cookie is no session", async () => {
    answers["GET /api/auth/status"] = [status(false, false)];
    answers["POST /api/auth/refresh"] = [refused(401, "not signed in")];
    const session = make();
    await session.start();
    expect(session.mode).toBe("signed-out");
    expect(session.passwordLogin).toBe(false);
    expect(session.token()).toBeUndefined();
    expect(timers).toHaveLength(0);
  });

  it("retries a refresh the service did not answer, and stays signed out meanwhile", async () => {
    answers["GET /api/auth/status"] = [status(false)];
    answers["POST /api/auth/refresh"] = ["throw"];
    const session = make();
    await session.start();
    expect(session.mode).toBe("signed-out");
    expect(timers.map((t) => t.ms)).toEqual([RETRY_MS]);
  });

  it("finds an invite link's secret, keeps it in memory and removes it from the address", async () => {
    answers["GET /api/auth/status"] = [status(false)];
    answers["POST /api/auth/refresh"] = [refused(401, "no")];
    const session = make({
      location: () => ({ pathname: "/invite", hash: "#abc%2Bdef" }),
    });
    await session.start();
    expect(session.invite).toBe("abc+def");
    expect(dropped).toBe(1);
  });

  it("ignores a fragment on any other page and an empty one", async () => {
    answers["GET /api/auth/status"] = [status(true), status(true)];
    const other = make({
      location: () => ({ pathname: "/inbox", hash: "#abc" }),
    });
    await other.start();
    expect(other.invite).toBeNull();
    const empty = make({
      location: () => ({ pathname: "/invite", hash: "#" }),
    });
    await empty.start();
    expect(empty.invite).toBeNull();
    expect(dropped).toBe(0);
  });
});

describe("signing in", () => {
  it("sends the name, the password and what this browser is called", async () => {
    answers["POST /api/auth/sign-in"] = [ok(grant("a"))];
    const session = make();
    expect(await session.signIn("Tijs", "correct horse battery")).toEqual({
      ok: true,
    });
    expect(calls[0]?.body).toEqual({
      name: "Tijs",
      password: "correct horse battery",
      device_name: "Firefox on Linux",
    });
    expect(session.mode).toBe("signed-in");
    expect(session.busy).toBe(false);
  });

  it("shows the service's words for a refusal and stays signed out", async () => {
    answers["POST /api/auth/sign-in"] = [
      refused(401, "wrong name or password"),
      refused(429, "too many sign-in attempts; try again in 30 seconds"),
      { error: {}, response: { status: 500, ok: false } },
      "throw",
    ];
    const session = make();
    expect(await session.signIn("a", "b")).toEqual({
      ok: false,
      message: "Wrong name or password.",
    });
    expect(await session.signIn("a", "b")).toEqual({
      ok: false,
      message: "Too many sign-in attempts; try again in 30 seconds.",
    });
    expect(await session.signIn("a", "b")).toEqual({
      ok: false,
      message: "Puddle refused that.",
    });
    expect(await session.signIn("a", "b")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
    expect(session.mode).not.toBe("signed-in");
    expect(session.token()).toBeUndefined();
  });

  it("does not send a second sign-in while one is on its way", async () => {
    let finish: (a: Answer) => void = () => undefined;
    const slow = {
      ...api,
      POST: () =>
        new Promise<Answer>((done) => {
          finish = done;
        }),
    };
    const session = make({ api: slow });
    const first = session.signIn("a", "b");
    expect(session.busy).toBe(true);
    expect(await session.signIn("a", "b")).toEqual({
      ok: false,
      message: "One moment.",
    });
    finish(ok(grant("a")));
    await first;
    expect(session.busy).toBe(false);
  });

  it("makes the owner account, and starts again when somebody else was first", async () => {
    answers["POST /api/auth/setup"] = [
      ok(grant("owner"), 201),
      refused(409, "the owner account is already set up"),
    ];
    answers["GET /api/auth/status"] = [status(false)];
    answers["POST /api/auth/refresh"] = [refused(401, "no")];
    const session = make();
    expect(await session.setup("Tijs", "correct horse battery")).toEqual({
      ok: true,
    });
    expect(session.mode).toBe("signed-in");
    const later = make();
    expect(await later.setup("mallory", "correct horse battery")).toEqual({
      ok: false,
      message: "The owner account is already set up.",
    });
    expect(later.mode).toBe("signed-out");
  });

  it("sets a password with an invite and forgets the secret", async () => {
    answers["GET /api/auth/status"] = [status(false)];
    answers["POST /api/auth/refresh"] = [refused(401, "no")];
    answers["POST /api/auth/invite"] = [
      refused(401, "this invite link is not valid any more"),
      ok(grant("a")),
    ];
    const session = make({
      location: () => ({ pathname: "/invite", hash: "#secret" }),
    });
    await session.start();
    expect(await session.redeemInvite("secret", "long enough pass")).toEqual({
      ok: false,
      message: "This invite link is not valid any more.",
    });
    expect(session.invite).toBe("secret");
    expect(await session.redeemInvite("secret", "long enough pass")).toEqual({
      ok: true,
    });
    expect(session.invite).toBeNull();
    expect(calls.at(-1)?.body).toMatchObject({ secret: "secret" });
  });
});

describe("staying signed in", () => {
  async function signedIn() {
    answers["POST /api/auth/sign-in"] = [ok(grant("one"))];
    const session = make();
    await session.signIn("a", "b");
    return session;
  }

  it("takes a new token when the timer fires and schedules the next", async () => {
    const session = await signedIn();
    answers["POST /api/auth/refresh"] = [ok(grant("two"))];
    timers.at(-1)?.run();
    await vi.waitFor(() => expect(session.token()).toBe("two"));
    expect(timers.filter((t) => !t.cleared && !t.fired)).toHaveLength(1);
  });

  it("drops to the sign-in page when the refresh is refused", async () => {
    const session = await signedIn();
    answers["POST /api/auth/refresh"] = [refused(401, "no")];
    timers.at(-1)?.run();
    await vi.waitFor(() => expect(session.mode).toBe("signed-out"));
    expect(session.token()).toBeUndefined();
    expect(session.user).toBeNull();
    expect(timers.filter((t) => !t.cleared && !t.fired)).toEqual([]);
  });

  it("keeps the old token and tries again when the service does not answer", async () => {
    const session = await signedIn();
    answers["POST /api/auth/refresh"] = ["throw"];
    timers.at(-1)?.run();
    await vi.waitFor(() => expect(timers.at(-1)?.ms).toBe(RETRY_MS));
    expect(session.mode).toBe("signed-in");
    expect(session.token()).toBe("one");
  });

  it("recovers once for several refused requests, and says whether it worked", async () => {
    const session = await signedIn();
    answers["POST /api/auth/refresh"] = [ok(grant("two"))];
    const [a, b] = await Promise.all([session.recover(), session.recover()]);
    expect([a, b]).toEqual([true, true]);
    expect(calls.filter((c) => c.path === "/api/auth/refresh")).toHaveLength(1);
    answers["POST /api/auth/refresh"] = [refused(401, "no")];
    expect(await session.recover()).toBe(false);
    expect(session.mode).toBe("signed-out");
    // Not signed in: nothing to recover.
    expect(await session.recover()).toBe(false);
  });

  it("never schedules sooner than a few seconds, whatever the clock says", async () => {
    answers["POST /api/auth/sign-in"] = [
      ok(grant("a", { expires_at: NOW - 5 })),
    ];
    const session = make();
    await session.signIn("a", "b");
    expect(timers[0]?.ms).toBe(5_000);
  });

  it("signs out: the service is told, the token and the timer go", async () => {
    const session = await signedIn();
    answers["POST /api/auth/sign-out"] = [
      { data: undefined, response: { status: 204, ok: true } },
    ];
    await session.signOut();
    expect(calls.at(-1)?.path).toBe("/api/auth/sign-out");
    expect(session.mode).toBe("signed-out");
    expect(session.token()).toBeUndefined();
    expect(timers.filter((t) => !t.cleared && !t.fired)).toEqual([]);
  });

  it("signs out here even when the service does not answer", async () => {
    const session = await signedIn();
    answers["POST /api/auth/sign-out"] = ["throw"];
    await session.signOut();
    expect(session.mode).toBe("signed-out");
    expect(session.token()).toBeUndefined();
  });

  it("stops its timer on request", async () => {
    const session = await signedIn();
    session.stop();
    expect(timers.at(-1)?.cleared).toBe(true);
    session.stop();
  });

  it("stays signed in when a refused request is recovered while the service is down", async () => {
    const session = await signedIn();
    answers["POST /api/auth/refresh"] = ["throw"];
    expect(await session.recover()).toBe(false);
    expect(session.mode).toBe("signed-in");
    answers["POST /api/auth/refresh"] = [refused(500, "broken")];
    expect(await session.recover()).toBe(false);
    expect(session.mode).toBe("signed-in");
  });

  it("holds the token in memory only", async () => {
    await signedIn();
    expect(JSON.stringify(Object.entries(localStorage))).not.toContain("one");
    expect(JSON.stringify(Object.entries(sessionStorage))).not.toContain("one");
    expect(document.cookie).not.toContain("one");
  });
});

describe("with the browser's own clock, timers, user agent and address", () => {
  it("finds the invite in the real address, removes it, and refreshes on a real timer", async () => {
    vi.useFakeTimers();
    try {
      history.replaceState(null, "", "/invite#the-secret");
      answers["GET /api/auth/status"] = [status(false)];
      answers["POST /api/auth/refresh"] = [refused(401, "no")];
      answers["POST /api/auth/sign-in"] = [
        ok(grant("one", { expires_at: Date.now() + 15 * 60_000 })),
      ];
      const session = new SessionStore({
        api: api as never,
        bootstrap: () => undefined,
      });
      await session.start();
      expect(session.invite).toBe("the-secret");
      expect(location.hash).toBe("");
      expect(location.pathname).toBe("/invite");
      await session.signIn("a", "b");
      expect(
        typeof (calls.at(-1)?.body as { device_name: string }).device_name,
      ).toBe("string");
      answers["POST /api/auth/refresh"] = [ok(grant("two"))];
      await vi.advanceTimersByTimeAsync(15 * 60_000);
      expect(session.token()).toBe("two");
      session.stop();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      history.replaceState(null, "", "/");
      vi.useRealTimers();
    }
  });

  it("reads the shell's token from the page when none is passed", async () => {
    (globalThis as { __PUDDLE__?: unknown }).__PUDDLE__ = { token: "shell" };
    try {
      const { session } = await import("./session.svelte.ts");
      await session.start();
      expect(session.mode).toBe("local");
    } finally {
      delete (globalThis as { __PUDDLE__?: unknown }).__PUDDLE__;
    }
  });
});
