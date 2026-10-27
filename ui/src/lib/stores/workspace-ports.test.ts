// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FakeEnvApi, port } from "#lib/testing/fake-environment.ts";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import { WorkspacePortsStore } from "./workspace-ports.svelte.ts";

let api: FakeEnvApi;
let source: FakeSource;
const make = () => new WorkspacePortsStore(api as never, source);

beforeEach(() => {
  api = new FakeEnvApi();
  source = new FakeSource();
  api.ports["web"] = { running: true, ports: [port(3000), port(5432)] };
});

describe("reading", () => {
  it("lists the ports and whether the workspace runs", async () => {
    const store = make();
    await store.load("web");
    expect(store.status).toBe("ready");
    expect(store.running).toBe(true);
    expect(store.ports.map((p) => p.guest_port)).toEqual([3000, 5432]);
    await store.load("docs");
    expect(store.running).toBe(false);
    expect(store.ports).toEqual([]);
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
    await refused.load("web", true);
    expect(refused.status).toBe("failed");
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
    api.ports["web"] = { running: true, ports: [port(8080)] };
    await store.load("web", true);
    release?.();
    await slow;
    expect(store.ports.map((p) => p.guest_port)).toEqual([8080]);
  });

  it("follows the events of its workspace and a resync, and stops", async () => {
    const store = make();
    const stop = store.start("web");
    await vi.waitFor(() => expect(store.status).toBe("ready"));
    api.ports["web"] = { running: true, ports: [port(1)] };
    source.emit({ type: "workspace_ports_changed", workspace: "docs" });
    await Promise.resolve();
    expect(store.ports).toHaveLength(2);
    source.emit({ type: "workspace_ports_changed", workspace: "web" });
    await vi.waitFor(() => expect(store.ports).toHaveLength(1));
    api.ports["web"] = { running: true, ports: [] };
    source.resync();
    await vi.waitFor(() => expect(store.ports).toHaveLength(0));
    stop();
    api.ports["web"] = { running: true, ports: [port(2)] };
    source.resync();
    await Promise.resolve();
    expect(store.ports).toHaveLength(0);
  });

  it("starts without a live source", async () => {
    const store = new WorkspacePortsStore(api as never, undefined);
    store.start("web")();
    await vi.waitFor(() => expect(store.status).toBe("ready"));
  });
});

describe("forwarding", () => {
  it("forwards to the same number or another, and keeps it or not", async () => {
    const store = make();
    await store.load("web");
    expect(await store.forward(3000, null, false)).toEqual({ ok: true });
    expect(store.ports[0]).toMatchObject({
      forwarded: true,
      host_port: 3000,
      pinned: false,
    });
    await store.forward(5432, 15432, true);
    expect(store.ports[1]).toMatchObject({ host_port: 15432, pinned: true });
    expect(api.bodies).toEqual([
      { host_port: null, pinned: false },
      { host_port: 15432, pinned: true },
    ]);
  });

  it("says why a forward is refused, and when the service is down", async () => {
    const store = make();
    await store.load("web");
    api.refuse = { status: 409, message: "port 3000 is in use by node" };
    expect(await store.forward(3000, null, false)).toEqual({
      ok: false,
      message: "Port 3000 is in use by node.",
    });
    const real = api.PUT;
    api.PUT = (async () => ({ error: undefined, response: {} })) as never;
    expect(await store.forward(3000, null, false)).toEqual({
      ok: false,
      message: "Puddle refused the forward.",
    });
    api.PUT = real;
    api.down = true;
    expect(await store.forward(3000, null, false)).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });

  it("stops a forward, counts one already gone as stopped, and reports a refusal", async () => {
    const store = make();
    await store.load("web");
    await store.forward(3000, null, true);
    expect(await store.release(3000)).toEqual({ ok: true });
    expect(store.ports[0]).toMatchObject({ forwarded: false, pinned: false });
    expect(await store.release(9999)).toEqual({ ok: true });
    api.refuse = { status: 500, message: "x" };
    expect(await store.release(3000)).toEqual({
      ok: false,
      message: "Puddle couldn't stop the forward.",
    });
    api.down = true;
    expect(await store.release(3000)).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });
});
