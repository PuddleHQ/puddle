// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import { FakeSshAgent, KEY_A } from "#lib/testing/fake-ssh-agent.ts";
import { WorkspaceSshAgentStore } from "./workspace-ssh-agent.svelte.ts";

let api: FakeSshAgent;
let source: FakeSource;
const make = () => new WorkspaceSshAgentStore(api as never, source);

beforeEach(() => {
  api = new FakeSshAgent();
  source = new FakeSource();
});

describe("reading", () => {
  it("is off for every host until one is turned on", async () => {
    const store = make();
    await store.load("web");
    expect(store.status).toBe("ready");
    expect(store.hosts).toEqual([]);
  });

  it("fails the first read and keeps the page on a quiet one", async () => {
    const store = make();
    api.down = true;
    await store.load("web");
    expect(store.status).toBe("failed");
    api.down = false;
    await store.load("web");
    api.down = true;
    await store.load("web", true);
    expect(store.status).toBe("ready");
    const refused = make();
    api.down = false;
    api.GET = (async () => ({
      response: { status: 500 } as Response,
    })) as never;
    await refused.load("web");
    expect(refused.status).toBe("failed");
    refused.status = "ready";
    await refused.load("web", true);
    expect(refused.status).toBe("ready");
  });

  it("never lets a slow answer overwrite a newer one", async () => {
    const store = make();
    const real = api.GET;
    let release: (() => void) | undefined;
    api.GET = (async (path: string, init: never) => {
      if (release === undefined) {
        await new Promise<void>((resolve) => (release = resolve));
      }
      return real(path, init);
    }) as never;
    const slow = store.load("web");
    await store.turnOn("build.example.com", null);
    release?.();
    await slow;
    expect(store.hosts.map((h) => h.host)).toEqual(["build.example.com"]);
  });

  it("follows a change of its own workspace's switch, a resync, and stops", async () => {
    const store = make();
    const stop = store.start("web");
    await vi.waitFor(() => expect(store.status).toBe("ready"));
    api.hosts["web"] = [{ host: "a.example.com", keys: null, changed_at: 1 }];
    source.emit({ type: "workspace_git_changed", workspace: "docs" });
    source.emit({ type: "something_else", workspace: "web" });
    source.emit(null);
    await Promise.resolve();
    expect(store.hosts).toEqual([]);
    source.emit({ type: "workspace_git_changed", workspace: "web" });
    await vi.waitFor(() => expect(store.hosts).toHaveLength(1));
    api.hosts["web"] = [];
    source.resync();
    await vi.waitFor(() => expect(store.hosts).toHaveLength(0));
    stop();
    api.hosts["web"] = [{ host: "b.example.com", keys: null, changed_at: 1 }];
    source.resync();
    await Promise.resolve();
    expect(store.hosts).toHaveLength(0);
  });
});

describe("the switch", () => {
  it("turns a host on for every key or for listed keys, and off again", async () => {
    const store = make();
    await store.load("web");
    expect(await store.turnOn("b.example.com", null)).toEqual({ ok: true });
    expect(await store.turnOn("a.example.com", [KEY_A])).toEqual({ ok: true });
    expect(store.hosts).toEqual([
      { host: "a.example.com", keys: [KEY_A], changed_at: 1 },
      { host: "b.example.com", keys: null, changed_at: 1 },
    ]);
    expect(await store.turnOff("a.example.com")).toEqual({ ok: true });
    expect(store.hosts.map((h) => h.host)).toEqual(["b.example.com"]);
  });

  it("counts a host that was already off as off", async () => {
    const store = make();
    await store.load("web");
    expect(await store.turnOff("gone.example.com")).toEqual({ ok: true });
  });

  it("says what the service refused, and when it is down", async () => {
    const store = make();
    await store.load("web");
    api.nextStatus = 422;
    expect(await store.turnOn("x", null)).toEqual({
      ok: false,
      message: "refused",
    });
    api.nextStatus = 500;
    expect(await store.turnOff("x")).toEqual({
      ok: false,
      message: "Puddle couldn't turn the agent off.",
    });
    api.down = true;
    const down = { ok: false, message: "Puddle's service isn't answering." };
    expect(await store.turnOn("x", null)).toEqual(down);
    expect(await store.turnOff("x")).toEqual(down);
    api.down = false;
    api.PUT = (async () => ({
      response: { status: 422 } as Response,
    })) as never;
    expect(await store.turnOn("x", null)).toEqual({
      ok: false,
      message: "Puddle refused the host.",
    });
  });
});
