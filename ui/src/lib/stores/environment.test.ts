// SPDX-License-Identifier: GPL-3.0-or-later
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FakeEnvApi, secret, variable } from "#lib/testing/fake-environment.ts";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import { EnvironmentStore } from "./environment.svelte.ts";

let api: FakeEnvApi;
let source: FakeSource;
const make = () => new EnvironmentStore(api as never, source);
const web = { kind: "workspace", name: "web" } as const;

beforeEach(() => {
  api = new FakeEnvApi();
  source = new FakeSource();
  api.global = [variable("EDITOR", { scope: "global", value: "vim" })];
  api.own["web"] = [variable("MODE", { value: "dev" })];
});
afterEach(() => vi.restoreAllMocks());

describe("reading", () => {
  it("lists the global variables, or a workspace's own with the global ones after them", async () => {
    const store = make();
    await store.load({ kind: "global" });
    expect(store.status).toBe("ready");
    expect(store.variables.map((v) => v.name)).toEqual(["EDITOR"]);
    await store.load(web);
    expect(store.variables.map((v) => v.name)).toEqual(["MODE", "EDITOR"]);
  });

  it("fails the first read and keeps the page on a quiet one", async () => {
    const store = make();
    api.down = true;
    await store.load(web);
    expect(store.status).toBe("failed");
    api.down = false;
    await store.load(web);
    api.down = true;
    await store.load(web, true);
    expect(store.status).toBe("ready");
    const refused = make();
    api.down = false;
    const real = api.GET;
    vi.spyOn(api, "GET").mockImplementation((async () => ({
      response: { status: 500 } as Response,
    })) as never);
    await refused.load(web);
    expect(refused.status).toBe("failed");
    await refused.load(web, true);
    expect(refused.status).toBe("failed");
    api.GET = real;
  });

  it("never lets a slow answer overwrite a newer one", async () => {
    const store = make();
    const real = api.GET;
    let release: (() => void) | undefined;
    vi.spyOn(api, "GET").mockImplementation((async (
      path: string,
      init: never,
    ) => {
      if (release === undefined) {
        await new Promise<void>((resolve) => (release = resolve));
      }
      return real(path, init);
    }) as never);
    const slow = store.load(web);
    api.own["web"] = [variable("MODE", { value: "prod" })];
    await store.load(web, true);
    release?.();
    await slow;
    expect(store.variables.find((v) => v.name === "MODE")?.value).toBe("prod");
  });

  it("follows a global change on both lists and a workspace's own change on its list, and stops", async () => {
    const store = make();
    const stop = store.start(web);
    await vi.waitFor(() => expect(store.status).toBe("ready"));
    api.own["web"] = [variable("MODE", { value: "prod" })];
    source.emit({ type: "workspace_env_changed", workspace: "docs" });
    await Promise.resolve();
    expect(store.variables[0]?.value).toBe("dev");
    source.emit({ type: "workspace_env_changed", workspace: "web" });
    await vi.waitFor(() => expect(store.variables[0]?.value).toBe("prod"));
    api.global = [];
    source.emit({ type: "global_env_changed" });
    await vi.waitFor(() => expect(store.variables).toHaveLength(1));
    api.own["web"] = [];
    source.resync();
    await vi.waitFor(() => expect(store.variables).toHaveLength(0));
    stop();
    api.own["web"] = [variable("LATE")];
    source.resync();
    await Promise.resolve();
    expect(store.variables).toHaveLength(0);
  });

  it("starts without a live source", async () => {
    const store = new EnvironmentStore(api as never, undefined);
    store.start({ kind: "global" })();
    await vi.waitFor(() => expect(store.status).toBe("ready"));
  });
});

describe("changing", () => {
  it("sets a plain variable in the global list or a workspace's", async () => {
    const store = make();
    await store.load({ kind: "global" });
    expect(await store.set("PAGER", { kind: "plain", value: "less" })).toEqual({
      ok: true,
    });
    expect(store.variables.map((v) => v.name)).toEqual(["EDITOR", "PAGER"]);
    await store.load(web);
    await store.set("MODE", { kind: "plain", value: "prod" });
    expect(store.variables.find((v) => v.name === "MODE")?.value).toBe("prod");
    expect(api.calls).toContain("PUT /api/env/{name}");
    expect(api.calls).toContain("PUT /api/workspaces/{id}/env/{name}");
  });

  it("sends a secret's value once and never gets it back", async () => {
    const store = make();
    await store.load(web);
    await store.set("TOKEN", {
      kind: "secret",
      hosts: ["api.example.com"],
      value: "s3cret",
    });
    const row = store.variables.find((v) => v.name === "TOKEN");
    expect(row?.kind).toBe("secret");
    expect(row?.value).toBeNull();
    expect(JSON.stringify(store.variables)).not.toContain("s3cret");
    // Changing only the hosts leaves the value out.
    await store.set("TOKEN", { kind: "secret", hosts: ["b.example.com"] });
    expect(store.variables.find((v) => v.name === "TOKEN")?.hosts).toEqual([
      "b.example.com",
    ]);
    expect(api.secretValues["TOKEN"]).toBe("s3cret");
  });

  it("says why a change is refused, in the service's words, and when the service is down", async () => {
    const store = make();
    await store.load(web);
    api.refuse = { status: 422, message: "PATH comes from the image" };
    expect(await store.set("PATH", { kind: "plain", value: "x" })).toEqual({
      ok: false,
      message: "PATH comes from the image.",
    });
    expect(await store.set("S", { kind: "secret", hosts: ["a.com"] })).toEqual({
      ok: false,
      message: "A new secret needs its value.",
    });
    api.down = true;
    expect(await store.set("A", { kind: "plain", value: "1" })).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });

  it("removes a variable, counts one that is gone as removed, and reports a refusal", async () => {
    const store = make();
    await store.load(web);
    expect(await store.remove("MODE")).toEqual({ ok: true });
    expect(store.variables.map((v) => v.name)).toEqual(["EDITOR"]);
    await store.load({ kind: "global" });
    expect(await store.remove("EDITOR")).toEqual({ ok: true });
    expect(store.variables).toEqual([]);
    api.refuse = { status: 500, message: "x" };
    expect(await store.remove("EDITOR")).toEqual({
      ok: false,
      message: "Puddle couldn't remove the variable.",
    });
    api.own["web"] = [secret("S", ["a.com"])];
    await store.load(web);
    const real = api.DELETE;
    vi.spyOn(api, "DELETE").mockImplementation((async () => ({
      response: { status: 404, ok: false },
    })) as never);
    expect(await store.remove("GONE")).toEqual({ ok: true });
    api.DELETE = real;
    api.down = true;
    expect(await store.remove("S")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });
});
