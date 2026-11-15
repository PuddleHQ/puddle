// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { FakeSettings } from "#lib/testing/fake-settings.ts";
import { AuditRetentionStore } from "./audit-retention.svelte.ts";

function fresh() {
  const api = new FakeSettings();
  return { api, store: new AuditRetentionStore(api as never) };
}

describe("the audit retention store", () => {
  it("reads the size", async () => {
    const { api, store } = fresh();
    expect(store.status).toBe("loading");
    await store.load();
    expect(store.status).toBe("ready");
    expect(store.size).toEqual(api.auditSize);
    expect(api.calls).toEqual(["GET /api/audit/retention"]);
  });

  it("fails with the reason when the read is refused or the service is down", async () => {
    const { api, store } = fresh();
    api.refuse.set("GET /api/audit/retention", {
      status: 403,
      error: "forbidden",
      message: "only the owner may do that",
    });
    await store.load();
    expect(store.status).toBe("failed");
    expect(store.reason).toBe("Only the owner may do that.");
    api.down = true;
    await store.load();
    expect(store.reason).toBe("Puddle's service isn't answering.");
  });

  it("saves the days and shows the numbers the answer carries", async () => {
    const { api, store } = fresh();
    await store.load();
    expect(await store.save(30)).toEqual({ ok: true });
    expect(api.bodies).toEqual([{ days: 30 }]);
    expect(store.size?.retention_days).toBe(30);
  });

  it("returns the server's refusal in its words and keeps the old days", async () => {
    const { api, store } = fresh();
    await store.load();
    api.refuse.set("PUT /api/audit/retention", {
      status: 422,
      error: "invalid",
      message: "the log is kept for 1 to 3650 days (0 given)",
    });
    expect(await store.save(0)).toEqual({
      ok: false,
      message: "The log is kept for 1 to 3650 days (0 given).",
    });
    expect(store.size?.retention_days).toBe(90);
  });

  it("says the service is down when a save does not arrive", async () => {
    const { api, store } = fresh();
    await store.load();
    api.down = true;
    expect(await store.save(30)).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });

  it("ignores an older read that answers after a newer one", async () => {
    const { store } = fresh();
    const first = store.load();
    await store.load();
    const size = store.size;
    await first;
    expect(store.size).toBe(size);
  });

  it("ignores an older read that fails after a newer one answered", async () => {
    let release: () => void = () => undefined;
    let calls = 0;
    const store = new AuditRetentionStore({
      GET: async () => {
        calls += 1;
        if (calls === 1) {
          await new Promise<void>((resolve) => (release = resolve));
          throw new TypeError("late failure");
        }
        return {
          data: new FakeSettings().auditSize,
          response: { ok: true, status: 200 },
        };
      },
      PUT: async () => ({ response: { ok: false, status: 500 } }),
    } as never);
    const first = store.load();
    await store.load();
    release();
    await first;
    expect(store.status).toBe("ready");
    expect(store.reason).toBeNull();
  });

  it("names a refusal that came without a message, and keeps a newer read's numbers over an older save", async () => {
    const answer = new FakeSettings().auditSize;
    let release: () => void = () => undefined;
    const store = new AuditRetentionStore({
      GET: async () => ({ data: answer, response: { ok: true, status: 200 } }),
      PUT: async (_path: string, init: { body: { days: number } }) => {
        if (init.body.days === 0) {
          return { response: { ok: false, status: 500 } };
        }
        await new Promise<void>((resolve) => (release = resolve));
        return {
          data: { ...answer, retention_days: init.body.days },
          response: { ok: true, status: 200 },
        };
      },
    } as never);
    expect(await store.save(0)).toEqual({
      ok: false,
      message: "Puddle refused that.",
    });
    const slow = store.save(7);
    await store.load();
    release();
    expect(await slow).toEqual({ ok: true });
    expect(store.size?.retention_days).toBe(90);
  });
});
