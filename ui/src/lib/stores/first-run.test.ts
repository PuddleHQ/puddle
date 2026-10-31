// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { FirstRunStore, type FirstRun } from "./first-run.svelte.ts";

const open: FirstRun = {
  completed: false,
  completed_at: null,
  dev_certificate: "not_checked",
};
const done: FirstRun = { ...open, completed: true, completed_at: 5 };

type Answer =
  { data?: unknown; error?: unknown; response: { status: number } } | "throw";
let answers: Answer[];
let bodies: unknown[];

const api = {
  GET: async () => {
    const answer = answers.shift() ?? { data: open, response: { status: 200 } };
    if (answer === "throw") throw new Error("down");
    return answer;
  },
  PUT: async (_path: string, init: { body: unknown }) => {
    bodies.push(init.body);
    const answer = answers.shift() ?? { data: done, response: { status: 200 } };
    if (answer === "throw") throw new Error("down");
    return answer;
  },
};
const trusted = { dev_certificate: "trusted", detail: null };
const post = async () => {
  const answer = answers.shift() ?? {
    data: trusted,
    response: { status: 200 },
  };
  if (answer === "throw") throw new Error("down");
  return answer;
};
const withPost = { ...api, POST: post };
const make = () => new FirstRunStore(withPost as never);

beforeEach(() => {
  answers = [];
  bodies = [];
});

describe("FirstRunStore", () => {
  it("reads the state", async () => {
    const store = make();
    expect(store.status).toBe("loading");
    await store.load();
    expect(store.status).toBe("ready");
    expect(store.state).toEqual(open);
  });

  it("fails when nothing was ever read, and keeps what it had otherwise", async () => {
    answers.push({ response: { status: 500 } });
    const a = make();
    await a.load();
    expect(a.status).toBe("failed");
    answers.push("throw");
    const b = make();
    await b.load();
    expect(b.status).toBe("failed");

    const c = make();
    await c.load();
    answers.push({ response: { status: 500 } }, "throw");
    await c.load();
    await c.load();
    expect(c.status).toBe("ready");
    expect(c.state).toEqual(open);
  });

  it("records that the flow is done", async () => {
    const store = make();
    expect(await store.complete()).toEqual({ ok: true });
    expect(bodies).toEqual([{ completed: true }]);
    expect(store.state).toEqual(done);
    expect(store.status).toBe("ready");
  });

  it("says why it could not record that", async () => {
    answers.push(
      {
        error: { error: "newer_settings", message: "newer" },
        response: { status: 409 },
      },
      {
        error: { error: "internal", message: "disk full" },
        response: { status: 500 },
      },
      { response: { status: 500 } },
      "throw",
    );
    const store = make();
    expect(await store.complete()).toEqual({
      ok: false,
      message:
        "These settings were saved by a newer Puddle. Update Puddle to change them.",
    });
    expect(await store.complete()).toEqual({ ok: false, message: "disk full" });
    expect(await store.complete()).toEqual({
      ok: false,
      message: "Puddle refused that.",
    });
    expect(await store.complete()).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
    expect(store.state).toBeNull();
  });

  it("trusts the certificate and keeps the new state and the system's words", async () => {
    const store = make();
    await store.load();
    answers.push({
      data: { dev_certificate: "trust_failed", detail: "os error 5" },
      response: { status: 200 },
    });
    const pending = store.trust();
    expect(store.trusting).toBe(true);
    await pending;
    expect(store.trusting).toBe(false);
    expect(store.state?.dev_certificate).toBe("trust_failed");
    expect(store.trustNote).toBe("os error 5");
    await store.trust();
    expect(store.state?.dev_certificate).toBe("trusted");
    expect(store.trustNote).toBeNull();
  });

  it("asks once at a time and says why a press did not work", async () => {
    const store = make();
    await store.load();
    const first = store.trust();
    await store.trust();
    await first;
    answers.push(
      {
        error: { error: "internal", message: "no thanks" },
        response: { status: 500 },
      },
      { response: { status: 500 } },
      "throw",
    );
    await store.trust();
    expect(store.trustNote).toBe("no thanks");
    await store.trust();
    expect(store.trustNote).toBe("Puddle refused that.");
    await store.trust();
    expect(store.trustNote).toBe("Puddle's service isn't answering.");
    expect(store.state?.dev_certificate).toBe("trusted");
  });

  it("keeps no state of its own when the state was never read", async () => {
    const store = make();
    await store.trust();
    expect(store.state).toBeNull();
  });
});
