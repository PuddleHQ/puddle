// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { user } from "#lib/testing/fake-admin.ts";
import { AdminStore } from "./admin.svelte.ts";

type Answer =
  | {
      data?: unknown;
      error?: unknown;
      response: { status: number; ok: boolean };
    }
  | "throw";
const good = (data: unknown, status = 200): Answer => ({
  data,
  response: { status, ok: true },
});
const bad = (status: number, message?: string): Answer => ({
  error: message === undefined ? undefined : { error: "x", message },
  response: { status, ok: false },
});

let answers: Record<string, Answer[]>;
let calls: { key: string; init: unknown }[];
const next = (key: string, init: unknown): Promise<Answer> => {
  calls.push({ key, init });
  const answer = answers[key]?.shift() ?? bad(500);
  if (answer === "throw") return Promise.reject(new Error("down"));
  return Promise.resolve(answer);
};
const api = {
  GET: (path: string, init?: unknown) => next(`GET ${path}`, init),
  POST: (path: string, init?: unknown) => next(`POST ${path}`, init),
  DELETE: (path: string, init?: unknown) => next(`DELETE ${path}`, init),
};
const make = () => new AdminStore(api as never);

const stats = {
  users: 2,
  workspaces: 3,
  running: 1,
  disk_used_mib: 10,
  memory_mib: 20,
  host_cpus: 4,
};
const reads = (users = [user()]) => {
  answers["GET /api/admin/users"] = [good({ users })];
  answers["GET /api/admin/stats"] = [good(stats)];
  answers["GET /api/admin/audit"] = [good({ lines: [] })];
};
const invite = { invite_secret: "s3", expires_at: 9, user: user() };

beforeEach(() => {
  answers = {};
  calls = [];
});

describe("probe", () => {
  it("is available when the server answers the stats", async () => {
    answers["GET /api/admin/stats"] = [good(stats)];
    const store = make();
    await store.probe();
    expect(store.available).toBe(true);
  });

  it("is not available on a 404 or a 403", async () => {
    answers["GET /api/admin/stats"] = [bad(404), bad(403)];
    const store = make();
    store.available = true;
    await store.probe();
    expect(store.available).toBe(false);
    store.available = true;
    await store.probe();
    expect(store.available).toBe(false);
  });

  it("keeps what it knew when the probe fails for another reason, and looks again on the next one", async () => {
    answers["GET /api/admin/stats"] = [
      bad(503),
      "throw",
      bad(401),
      good(stats),
    ];
    const store = make();
    await store.probe();
    await store.probe();
    await store.probe();
    expect(store.available).toBe(false);
    await store.probe();
    expect(store.available).toBe(true);
    answers["GET /api/admin/stats"] = [bad(500), "throw"];
    await store.probe();
    await store.probe();
    expect(store.available).toBe(true);
  });
});

describe("load", () => {
  it("reads the users, the totals and the audit", async () => {
    reads([user(), user({ id: 3, name: "Kim" })]);
    const store = make();
    expect(store.status).toBe("loading");
    await store.load();
    expect(store.status).toBe("ready");
    expect(store.available).toBe(true);
    expect(store.users.map((u) => u.name)).toEqual(["Sam", "Kim"]);
    expect(store.stats?.workspaces).toBe(3);
  });

  it("says there is no screen on a 404, and why on another failure", async () => {
    answers["GET /api/admin/users"] = [bad(404)];
    answers["GET /api/admin/stats"] = [bad(404)];
    answers["GET /api/admin/audit"] = [bad(404)];
    const store = make();
    store.available = true;
    await store.load();
    expect(store.status).toBe("failed");
    expect(store.available).toBe(false);
    expect(store.reason).toContain("no admin screen");

    answers["GET /api/admin/users"] = [good({ users: [] })];
    answers["GET /api/admin/stats"] = [good(stats)];
    answers["GET /api/admin/audit"] = [bad(500)];
    await store.load();
    expect(store.reason).toContain("status 500");
  });

  it("names a service that does not answer", async () => {
    answers["GET /api/admin/users"] = ["throw"];
    answers["GET /api/admin/stats"] = [good(stats)];
    answers["GET /api/admin/audit"] = [good({ lines: [] })];
    const store = make();
    await store.load();
    expect(store.status).toBe("failed");
    expect(store.reason).toContain("isn't answering");
  });

  it("lets a newer read speak when an older one fails", async () => {
    const store = make();
    answers["GET /api/admin/users"] = ["throw"];
    answers["GET /api/admin/stats"] = [good(stats)];
    answers["GET /api/admin/audit"] = [good({ lines: [] })];
    const first = store.load();
    reads([user({ name: "Second" })]);
    await store.load();
    await first;
    expect(store.status).toBe("ready");
    expect(store.users[0]?.name).toBe("Second");
  });

  it("starts the audit read only once the users are in", async () => {
    let releaseUsers: (answer: Answer) => void = () => undefined;
    const held = new Promise<Answer>((resolve) => {
      releaseUsers = resolve;
    });
    const store = make();
    reads();
    const original = api.GET;
    api.GET = (path: string, init?: unknown) =>
      path === "/api/admin/users" ? held : original(path, init);
    try {
      const loading = store.load();
      await Promise.resolve();
      expect(calls.map((call) => call.key)).not.toContain(
        "GET /api/admin/audit",
      );
      releaseUsers(good({ users: [user()] }));
      await loading;
      expect(calls.map((call) => call.key)).toContain("GET /api/admin/audit");
    } finally {
      api.GET = original;
    }
  });

  it("shows the newest read when two overlap", async () => {
    const store = make();
    reads([user({ name: "First" })]);
    const first = store.load();
    answers["GET /api/admin/users"]?.push(
      good({ users: [user({ name: "Second" })] }),
    );
    answers["GET /api/admin/stats"]?.push(good(stats));
    answers["GET /api/admin/audit"]?.push(good({ lines: [] }));
    await store.load();
    await first;
    expect(store.users[0]?.name).toBe("Second");
  });
});

describe("create and reissue", () => {
  it("hands the invite back once and reads the list again", async () => {
    answers["POST /api/admin/users"] = [good(invite, 201)];
    reads();
    const store = make();
    const made = await store.create("Sam");
    expect(made).toEqual({ ok: true, invite });
    expect(calls[0]?.init).toEqual({ body: { name: "Sam" } });
    expect(store.status).toBe("ready");
    expect(JSON.stringify(store.users)).not.toContain("s3");
  });

  it("says why a create is refused, or that the service is down", async () => {
    answers["POST /api/admin/users"] = [
      bad(409, "that name is taken"),
      "throw",
      bad(500),
    ];
    const store = make();
    expect(await store.create("Sam")).toEqual({
      ok: false,
      message: "That name is taken.",
    });
    expect(await store.create("Sam")).toMatchObject({ ok: false });
    expect(await store.create("Sam")).toEqual({
      ok: false,
      message: "Puddle refused that.",
    });
  });

  it("reissues for a user", async () => {
    answers["POST /api/admin/users/{id}/invite"] = [
      good(invite),
      bad(409, "no"),
      "throw",
    ];
    reads();
    const store = make();
    expect(await store.reissue(2)).toEqual({ ok: true, invite });
    expect(calls[0]?.init).toEqual({ params: { path: { id: 2 } } });
    expect(await store.reissue(1)).toEqual({ ok: false, message: "No." });
    expect(await store.reissue(1)).toMatchObject({ ok: false });
  });
});

describe("remove", () => {
  it("sends the typed name and reads the list again", async () => {
    answers["DELETE /api/admin/users/{id}"] = [good(undefined, 202)];
    reads();
    const store = make();
    expect(await store.remove(2, "Sam")).toEqual({ ok: true });
    expect(calls[0]?.init).toEqual({
      params: { path: { id: 2 }, query: { confirm: "Sam" } },
    });
    expect(store.status).toBe("ready");
  });

  it("treats a user that is gone already as deleted", async () => {
    answers["DELETE /api/admin/users/{id}"] = [bad(404)];
    reads();
    expect(await make().remove(2, "Sam")).toEqual({ ok: true });
  });

  it("reports a refusal and a service that is down", async () => {
    answers["DELETE /api/admin/users/{id}"] = [
      bad(422, "the name does not match"),
      "throw",
    ];
    const store = make();
    expect(await store.remove(2, "Sa")).toEqual({
      ok: false,
      message: "The name does not match.",
    });
    expect(await store.remove(2, "Sam")).toMatchObject({ ok: false });
  });
});
