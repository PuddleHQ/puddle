// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FakeEnvApi } from "#lib/testing/fake-environment.ts";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import { ShellInitStore } from "./workspace-shell-init.svelte.ts";

let api: FakeEnvApi;
let source: FakeSource;
const make = () => new ShellInitStore(api as never, source);

beforeEach(() => {
  api = new FakeEnvApi();
  source = new FakeSource();
});

describe("reading", () => {
  it("reads the saved script, or says none is saved", async () => {
    const store = make();
    await store.load("web");
    expect(store.view).toMatchObject({ saved: false, content: "" });
    api.shell["web"] = { content: "export A=1\n", changed_at: 5 };
    await store.load("web");
    expect(store.view).toMatchObject({ saved: true, content: "export A=1\n" });
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
    api.shell["web"] = { content: "new", changed_at: 9 };
    await store.load("web", true);
    release?.();
    await slow;
    expect(store.view?.content).toBe("new");
  });

  it("follows a change of its workspace, notes a restore, and stops", async () => {
    const store = make();
    store.restored = true;
    const stop = store.start("web");
    expect(store.restored).toBe(false);
    await vi.waitFor(() => expect(store.status).toBe("ready"));
    api.shell["web"] = { content: "x", changed_at: 3 };
    source.emit({ type: "workspace_shell_init_changed", workspace: "docs" });
    await Promise.resolve();
    expect(store.view?.saved).toBe(false);
    source.emit({ type: "workspace_shell_init_changed", workspace: "web" });
    await vi.waitFor(() => expect(store.view?.saved).toBe(true));
    source.emit({ type: "workspace_shell_init_restored", workspace: "web" });
    expect(store.restored).toBe(true);
    api.shell["web"] = { content: "y", changed_at: 4 };
    source.resync();
    await vi.waitFor(() => expect(store.view?.content).toBe("y"));
    stop();
    api.shell["web"] = { content: "z", changed_at: 5 };
    source.resync();
    await Promise.resolve();
    expect(store.view?.content).toBe("y");
  });

  it("starts without a live source", async () => {
    const store = new ShellInitStore(api as never, undefined);
    store.start("web")();
    await vi.waitFor(() => expect(store.status).toBe("ready"));
  });
});

describe("changing", () => {
  it("saves a script and says whether the workspace ran it", async () => {
    const store = make();
    await store.load("web");
    const result = await store.save("export A=1");
    expect(result).toMatchObject({ ok: true, value: { checked: true } });
    expect(store.view).toMatchObject({ saved: true, content: "export A=1" });
    api.shellChecked = { checked: false, reason: "It is stopped." };
    expect(await store.save("export A=2")).toMatchObject({
      ok: true,
      value: { checked: false, unchecked_reason: "It is stopped." },
    });
  });

  it("gives the refusal as the service wrote it, and says when the service is down", async () => {
    const store = make();
    await store.load("web");
    api.refuse = { status: 422, message: "it printed:\nhello" };
    expect(await store.save("echo hello")).toEqual({
      ok: false,
      message: "it printed:\nhello",
    });
    const real = api.PUT;
    api.PUT = (async () => ({ error: undefined, response: {} })) as never;
    expect(await store.save("x")).toEqual({
      ok: false,
      message: "Puddle refused the script.",
    });
    api.PUT = real;
    api.down = true;
    expect(await store.save("x")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });

  it("removes the script, counts one already gone as removed, and reports a refusal", async () => {
    const store = make();
    api.shell["web"] = { content: "x", changed_at: 1 };
    await store.load("web");
    expect(await store.remove()).toEqual({ ok: true });
    expect(store.view?.saved).toBe(false);
    expect(await store.remove()).toEqual({ ok: true });
    api.refuse = { status: 500, message: "x" };
    api.shell["web"] = { content: "x", changed_at: 1 };
    expect(await store.remove()).toEqual({
      ok: false,
      message: "Puddle couldn't remove the script.",
    });
    api.down = true;
    expect(await store.remove()).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });
});
