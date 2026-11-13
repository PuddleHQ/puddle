// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { DevicesStore, type Device } from "./devices.svelte.ts";

type Answer =
  | {
      data?: unknown;
      error?: unknown;
      response: { status: number; ok: boolean };
    }
  | "throw";

const device = (id: number, over: Partial<Device> = {}): Device => ({
  id,
  name: `device ${id}`,
  created_at: 1,
  last_seen_at: 2,
  expires_at: 3,
  current: false,
  ...over,
});
const good = (data: unknown, status = 200): Answer => ({
  data,
  response: { status, ok: true },
});
const bad = (status: number, message?: string): Answer => ({
  error: message === undefined ? undefined : { error: "x", message },
  response: { status, ok: false },
});

let answers: Record<string, Answer[]>;
let calls: string[];
const next = (key: string): Promise<Answer> => {
  calls.push(key);
  const answer = answers[key]?.shift() ?? bad(500);
  if (answer === "throw") return Promise.reject(new Error("down"));
  return Promise.resolve(answer);
};
const api = {
  GET: (path: string) => next(`GET ${path}`),
  PUT: (path: string) => next(`PUT ${path}`),
  DELETE: (path: string) => next(`DELETE ${path}`),
};
const make = () => new DevicesStore(api as never);
const methods = (enabled: boolean) => ({
  methods: [{ method: "password", enabled }],
});

beforeEach(() => {
  answers = {};
  calls = [];
});

describe("load", () => {
  it("reads the devices and the sign-in methods", async () => {
    answers["GET /api/devices"] = [good({ devices: [device(1), device(2)] })];
    answers["GET /api/auth/methods"] = [good(methods(true))];
    const store = make();
    expect(store.status).toBe("loading");
    await store.load();
    expect(store.status).toBe("ready");
    expect(store.devices.map((d) => d.id)).toEqual([1, 2]);
    expect(store.methods).toEqual(methods(true).methods);
  });

  it("has no methods for someone who may not read them", async () => {
    answers["GET /api/devices"] = [good({ devices: [] })];
    answers["GET /api/auth/methods"] = [bad(403, "only the owner")];
    const store = make();
    await store.load();
    expect(store.status).toBe("ready");
    expect(store.methods).toEqual([]);
  });

  it("says why the devices could not be read, and is not fooled by a methods read that throws", async () => {
    answers["GET /api/devices"] = [bad(500), "throw"];
    answers["GET /api/auth/methods"] = ["throw", "throw"];
    const store = make();
    await store.load();
    expect(store.status).toBe("failed");
    expect(store.reason).toBe("Puddle's service hit an error (status 500).");
    await store.load();
    expect(store.reason).toBe("Puddle's service isn't answering.");
    expect(store.methods).toEqual([]);
  });

  it("lets the newest read win", async () => {
    let release: (a: Answer) => void = () => undefined;
    let first = true;
    const slow = {
      ...api,
      GET: (path: string) => {
        if (path === "/api/devices" && first) {
          first = false;
          return new Promise<Answer>((done) => {
            release = done;
          });
        }
        return next(`GET ${path}`);
      },
    };
    answers["GET /api/devices"] = [good({ devices: [device(2)] })];
    answers["GET /api/auth/methods"] = [
      good(methods(true)),
      good(methods(true)),
    ];
    const store = new DevicesStore(slow as never);
    const older = store.load();
    await store.load();
    release(good({ devices: [device(1)] }));
    await older;
    expect(store.devices.map((d) => d.id)).toEqual([2]);
  });
});

describe("revoke", () => {
  async function loaded() {
    answers["GET /api/devices"] = [good({ devices: [device(1), device(2)] })];
    answers["GET /api/auth/methods"] = [good(methods(true))];
    const store = make();
    await store.load();
    return store;
  }

  it("takes the device out of the list", async () => {
    const store = await loaded();
    answers["DELETE /api/devices/{id}"] = [good(undefined, 204)];
    expect(await store.revoke(1)).toEqual({ ok: true });
    expect(store.devices.map((d) => d.id)).toEqual([2]);
  });

  it("counts a device that is gone already as revoked and reads the list again", async () => {
    const store = await loaded();
    answers["DELETE /api/devices/{id}"] = [bad(404, "no device 1")];
    answers["GET /api/devices"] = [good({ devices: [device(2)] })];
    answers["GET /api/auth/methods"] = [good(methods(true))];
    expect(await store.revoke(1)).toEqual({ ok: true });
    expect(store.devices.map((d) => d.id)).toEqual([2]);
  });

  it("keeps the device and says why when the service refuses or does not answer", async () => {
    const store = await loaded();
    answers["DELETE /api/devices/{id}"] = [bad(500), "throw"];
    expect(await store.revoke(1)).toEqual({
      ok: false,
      message: "Puddle's service hit an error (status 500).",
    });
    expect(await store.revoke(1)).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
    expect(store.devices).toHaveLength(2);
  });
});

describe("methods and the password", () => {
  it("switches a method and shows the answer", async () => {
    answers["PUT /api/auth/methods"] = [good(methods(false))];
    const store = make();
    expect(await store.setMethod("password", false)).toEqual({ ok: true });
    expect(store.methods).toEqual(methods(false).methods);
  });

  it("shows the service's refusal to leave no way to sign in", async () => {
    answers["PUT /api/auth/methods"] = [
      bad(422, "at least one way of signing in must stay on"),
      bad(500),
      "throw",
    ];
    const store = make();
    expect(await store.setMethod("password", false)).toEqual({
      ok: false,
      message: "At least one way of signing in must stay on.",
    });
    expect(await store.setMethod("password", false)).toEqual({
      ok: false,
      message: "Puddle refused that.",
    });
    expect(await store.setMethod("password", false)).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });

  it("changes the password and reads the devices again, or says why not", async () => {
    answers["PUT /api/auth/password"] = [
      good(undefined, 204),
      bad(401, "wrong name or password"),
      bad(500),
      "throw",
    ];
    answers["GET /api/devices"] = [good({ devices: [device(1)] })];
    answers["GET /api/auth/methods"] = [good(methods(true))];
    const store = make();
    expect(await store.changePassword("old", "new")).toEqual({ ok: true });
    expect(store.devices).toHaveLength(1);
    expect(await store.changePassword("old", "new")).toEqual({
      ok: false,
      message: "Wrong name or password.",
    });
    expect(await store.changePassword("old", "new")).toEqual({
      ok: false,
      message: "Puddle refused that.",
    });
    expect(await store.changePassword("old", "new")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });
});
