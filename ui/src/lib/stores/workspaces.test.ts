// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import {
  FakeWorkspaces,
  cleanCheck,
  workspace,
} from "#lib/testing/fake-workspaces.ts";
import { WorkspaceStore, fieldOf, type Settled } from "./workspaces.svelte.ts";

let api: FakeWorkspaces;
let source: FakeSource;
let store: WorkspaceStore;
let settled: Settled[];

beforeEach(() => {
  api = new FakeWorkspaces();
  api.list = [workspace("b-site"), workspace("a-shop", { status: "running" })];
  source = new FakeSource();
  settled = [];
  store = new WorkspaceStore({
    api: api as never,
    source,
    now: () => 777,
    pollMs: 1000,
  });
  store.onSettled = (s) => settled.push(s);
});
afterEach(() => vi.useRealTimers());

const status = (workspace: string, state: string) => ({
  type: "status_changed",
  workspace,
  status: state,
});
const step = (
  workspace: string,
  name: string,
  detail: string | null = null,
) => ({
  type: "workspace_progress",
  workspace,
  step: name,
  detail,
});

describe("reading the list", () => {
  it("loads it sorted by name", async () => {
    expect(store.status).toBe("loading");
    await store.refresh();
    expect(store.status).toBe("ready");
    expect(store.list.map((w) => w.name)).toEqual(["a-shop", "b-site"]);
    expect(store.byId("w-a-shop")?.status).toBe("running");
    expect(store.byId("w-nope")).toBeUndefined();
  });

  it("fails quietly while it has never worked, and keeps what it has after", async () => {
    api.down = true;
    await store.refresh();
    expect(store.status).toBe("failed");
    expect(store.loadError).toBe("Puddle's service isn't answering.");
    api.down = false;
    await store.refresh();
    expect(store.status).toBe("ready");
    expect(store.loadError).toBeNull();
    api.down = true;
    await store.refresh();
    expect(store.status).toBe("ready");
    expect(store.list).toHaveLength(2);
    expect(store.loadError).toBe("Puddle's service isn't answering.");
  });

  it("fails on a refusal as well as on a dead service", async () => {
    api.refuse.set("GET /api/workspaces", { status: 500, message: "broken" });
    await store.refresh();
    expect(store.status).toBe("failed");
    expect(store.loadError).toBe("Broken.");
    await store.refresh();
    api.refuse.set("GET /api/workspaces", { status: 500, message: "broken" });
    await store.refresh();
    expect(store.status).toBe("ready");
  });

  it("shares overlapping reads, then reads once more", async () => {
    const first = store.refresh();
    const second = store.refresh();
    expect(second).toBe(first);
    await first;
    await vi.waitFor(() =>
      expect(api.calls.filter((c) => c === "GET /api/workspaces")).toHaveLength(
        2,
      ),
    );
  });
});

describe("events", () => {
  beforeEach(async () => {
    await store.refresh();
  });

  it("applies a status change in place and keeps the order", () => {
    store.handleEvent(status("w-b-site", "running"));
    expect(store.byId("w-b-site")?.status).toBe("running");
    expect(store.list.map((w) => w.name)).toEqual(["a-shop", "b-site"]);
  });

  it("reads the list when a workspace it doesn't know changes", async () => {
    api.list.push(workspace("c-new", { status: "created" }));
    store.handleEvent(status("w-c-new", "starting"));
    await vi.waitFor(() => expect(store.byId("w-c-new")).toBeDefined());
  });

  it("reads the list again when a workspace's repositories change", async () => {
    api.list = api.list.map((w) =>
      w.id === "w-a-shop"
        ? {
            ...w,
            repositories: [
              {
                entry: "e-x",
                url: "https://github.com/acme/x.git",
                directory: "x",
              },
            ],
          }
        : w,
    );
    store.handleEvent({
      type: "workspace_repositories_changed",
      workspace: "w-a-shop",
    });
    await vi.waitFor(() =>
      expect(store.byId("w-a-shop")?.repositories[0]?.directory).toBe("x"),
    );
  });

  it("remembers the last out-of-memory kill with the time the page heard it", () => {
    store.handleEvent({
      type: "oom_kill",
      workspace: "w-a-shop",
      pid: 9,
      process: "node",
    });
    expect(store.oom["w-a-shop"]).toEqual({ process: "node", pid: 9, at: 777 });
  });

  it("keeps the last tool install state of a workspace and drops it when the workspace goes down", () => {
    store.handleEvent({
      type: "workspace_tools",
      workspace: "w-a-shop",
      state: { phase: "installing", done: 0, total: 2, tools: ["node"] },
    });
    store.handleEvent({
      type: "workspace_tools",
      workspace: "w-a-shop",
      state: { phase: "ready" },
    });
    expect(store.tools["w-a-shop"]).toEqual({ phase: "ready" });
    store.handleEvent({
      type: "status_changed",
      workspace: "w-a-shop",
      status: "stopped",
    });
    expect(store.tools["w-a-shop"]).toBeUndefined();
  });

  it("reads the list again when a disk reaches the warning threshold", async () => {
    api.list = api.list.map((w) =>
      w.id === "w-a-shop"
        ? { ...w, disk_used_mib: 31_000, disk_warnings: ["volume" as const] }
        : w,
    );
    store.handleEvent({
      type: "disk_nearly_full",
      workspace: "w-a-shop",
      disk: "volume",
      percent: 94,
    });
    await vi.waitFor(() =>
      expect(store.byId("w-a-shop")?.disk_warnings).toEqual(["volume"]),
    );
  });

  it("ignores events about nothing it shows", () => {
    store.handleEvent({ type: "rules_changed" });
    store.handleEvent("nonsense");
    expect(store.progress).toEqual({});
  });

  it("shows a step, and the record's operation with it", async () => {
    api.list = api.list.map((w) =>
      w.id === "w-b-site" ? { ...w, busy: "starting" } : w,
    );
    await store.refresh();
    store.handleEvent(step("w-b-site", "syncing", "files"));
    expect(store.progress["w-b-site"]).toEqual({
      step: "syncing",
      detail: "files",
      repository: null,
      operation: "starting",
      failed: false,
    });
  });

  it("keeps the checkout a cloning step names", async () => {
    await store.refresh();
    store.handleEvent({
      ...step("w-b-site", "cloning", "https://x/api.git"),
      repository: "api",
    });
    expect(store.progress["w-b-site"]?.repository).toBe("api");
    store.handleEvent({
      ...step("w-b-site", "failed", "no"),
      repository: "api",
    });
    expect(store.progress["w-b-site"]?.repository).toBe("api");
  });

  it("reads the record when progress arrives for a workspace that isn't marked busy", async () => {
    const before = api.calls.length;
    store.handleEvent(step("w-b-site", "starting"));
    await vi.waitFor(() => expect(api.calls.length).toBeGreaterThan(before));
  });

  it("clears the step on done, reads the list and tells who finished what", async () => {
    store.handleEvent(step("w-b-site", "starting"));
    await store.refresh(); // the step made it read the record; events wait for the read
    api.list = api.list.map((w) =>
      w.id === "w-b-site" ? { ...w, busy: null, status: "running" } : w,
    );
    store.handleEvent(step("w-b-site", "done"));
    expect(store.progress["w-b-site"]).toBeUndefined();
    await vi.waitFor(() =>
      expect(store.byId("w-b-site")?.status).toBe("running"),
    );
    expect(settled).toEqual([
      {
        id: "w-b-site",
        label: "b-site",
        operation: "starting",
        failed: false,
        detail: null,
      },
    ]);
  });

  it("keeps a failure, with its reason, until it is dismissed", () => {
    store.handleEvent(step("w-b-site", "failed", "no boot"));
    expect(store.progress["w-b-site"]).toMatchObject({
      step: "failed",
      detail: "no boot",
      failed: true,
    });
    expect(settled[0]).toMatchObject({ failed: true, detail: "no boot" });
    store.dismissProgress("w-b-site");
    expect(store.progress["w-b-site"]).toBeUndefined();
  });

  it("applies events that arrive during a read after it, so the read can't undo them", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    const slow = api.GET;
    api.GET = async (
      path: string,
      init?: { params?: { path?: Record<string, unknown> } },
    ) => {
      const result = await slow(path, init);
      await gate;
      return result;
    };
    const reading = store.refresh();
    await vi.waitFor(() => expect(api.calls.length).toBeGreaterThan(1));
    store.handleEvent(status("w-b-site", "running"));
    expect(store.byId("w-b-site")?.status).toBe("stopped");
    release();
    await reading;
    expect(store.byId("w-b-site")?.status).toBe("running");
  });
});

describe("actions", () => {
  beforeEach(async () => {
    await store.refresh();
  });

  it("creates a workspace and lists it as busy", async () => {
    store.progress = {
      "w-new-one": {
        step: "failed",
        detail: "old",
        repository: null,
        operation: "creating",
        failed: true,
      },
    };
    const result = await store.create({
      name: "new-one",
      repositories: [{ url: "https://github.com/acme/new-one.git" }],
    });
    expect(result).toMatchObject({
      ok: true,
      value: { name: "new-one", busy: "creating" },
    });
    expect(store.byId("w-new-one")?.busy).toBe("creating");
    expect(store.progress["w-new-one"]).toBeUndefined();
    expect(api.bodies.at(-1)).toEqual({
      name: "new-one",
      repositories: [{ url: "https://github.com/acme/new-one.git" }],
    });
  });

  it("puts a name that is taken on the name field", async () => {
    const result = await store.create({
      name: "a-shop",
      repositories: [{ url: "https://x.org/a.git" }],
    });
    expect(result).toMatchObject({
      ok: false,
      reason: "conflict",
      field: "name",
    });
  });

  it("puts a refusal on the field it is about", async () => {
    api.refuse.set("POST /api/workspaces", {
      status: 422,
      message:
        "SSH remotes are not supported yet; use the repository's HTTPS URL instead",
    });
    const result = await store.create({
      name: "x",
      repositories: [{ url: "git@x:y" }],
    });
    expect(result).toMatchObject({ ok: false, field: "repo_url" });
  });

  it("says the service is down", async () => {
    api.down = true;
    expect(
      await store.create({ name: "x", repositories: [{ url: "https://x/y" }] }),
    ).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });

  it.each([
    ["startWorkspace", "starting", "starting"],
    ["stopWorkspace", "draining", "stopping"],
    ["reclaim", "stopped", "reclaiming"],
    ["resetRootDisk", "stopped", "resetting_root_disk"],
  ] as const)(
    "%s marks the workspace busy at once",
    async (method, state, busy) => {
      store.progress = {
        "w-b-site": {
          step: "failed",
          detail: null,
          repository: null,
          operation: null,
          failed: true,
        },
      };
      const result = await store[method]("w-b-site");
      expect(result.ok).toBe(true);
      const w = store.byId("w-b-site");
      expect(w?.busy).toBe(busy);
      expect(w?.status).toBe(state);
      expect(store.progress["w-b-site"]).toBeUndefined();
    },
  );

  it("growRootDisk sends the size in MiB and marks the workspace busy at once", async () => {
    api.list = [workspace("b-site", { root_disk_size_mib: 10_240 })];
    await store.refresh();
    const result = await store.growRootDisk("w-b-site", 20_480);
    expect(result.ok).toBe(true);
    expect(api.bodies.at(-1)).toEqual({ size_mib: 20_480 });
    expect(store.byId("w-b-site")).toMatchObject({
      busy: "growing_root_disk",
      root_disk_size_mib: 20_480,
    });
  });

  it("growRootDisk hands a refusal back and leaves the workspace alone", async () => {
    api.list = [workspace("b-site", { root_disk_size_mib: 10_240 })];
    await store.refresh();
    const result = await store.growRootDisk("w-b-site", 10_240);
    expect(result).toMatchObject({ ok: false });
    expect(store.byId("w-b-site")?.busy).toBeNull();
  });

  it("takes the operation from its first step when the answer to the request is late", async () => {
    // The service finished before its answer reached the page: steps, then done.
    const real = api.POST;
    api.POST = async (path: string, init: Parameters<typeof real>[1]) => {
      const answer = await real(path, init);
      store.handleEvent(step("w-b-site", "starting"));
      store.handleEvent(step("w-b-site", "syncing"));
      store.handleEvent(status("w-b-site", "running"));
      api.list = api.list.map((w) =>
        w.id === "w-b-site" ? { ...w, busy: null, status: "running" } : w,
      );
      store.handleEvent(step("w-b-site", "done"));
      return answer;
    };
    await store.startWorkspace("w-b-site");
    await store.refresh();
    // The answer said "busy starting"; it is older than the end of the operation, so it is not shown.
    expect(store.byId("w-b-site")).toMatchObject({
      busy: null,
      status: "running",
    });
    expect(settled.at(-1)).toMatchObject({
      id: "w-b-site",
      operation: "starting",
      failed: false,
    });
  });

  it("keeps the steps of a new operation but forgets the last one's failure", async () => {
    store.progress = {
      "w-b-site": {
        step: "failed",
        detail: "old",
        repository: null,
        operation: "starting",
        failed: true,
      },
    };
    await store.startWorkspace("w-b-site");
    expect(store.progress["w-b-site"]).toBeUndefined();
    store.handleEvent(step("w-b-site", "syncing"));
    await store.stopWorkspace("w-b-site");
    expect(store.progress["w-b-site"]).toMatchObject({
      step: "syncing",
      failed: false,
    });
  });

  it("keeps a failure that arrives before the answer to the request that started it", async () => {
    // The service failed fast and the stream beat the answer: this failure is the new one.
    const real = api.POST;
    api.POST = async (path: string, init: Parameters<typeof real>[1]) => {
      const answer = await real(path, init);
      store.handleEvent(step("w-b-site", "starting"));
      store.handleEvent(step("w-b-site", "failed", "boom"));
      return answer;
    };
    await store.startWorkspace("w-b-site");
    expect(store.progress["w-b-site"]).toMatchObject({
      step: "failed",
      detail: "boom",
      failed: true,
    });
  });

  it("reads the list again after a 409 and after a 404, and says which", async () => {
    api.refuse.set("POST /api/workspaces/{id}/start", {
      status: 409,
      message: "already running",
    });
    const conflict = await store.startWorkspace("w-b-site");
    expect(conflict).toEqual({
      ok: false,
      message: "already running",
      reason: "conflict",
    });
    const gone = await store.startWorkspace("w-ghost");
    expect(gone).toEqual({
      ok: false,
      message: "That workspace no longer exists.",
      reason: "gone",
    });
  });

  it("reads what a delete would lose", async () => {
    api.check = cleanCheck("w-b-site");
    const result = await store.checkDelete("w-b-site");
    expect(result).toMatchObject({
      ok: true,
      value: { fingerprint: "fp-clean" },
    });
    expect(await store.checkDelete("w-ghost")).toMatchObject({
      ok: false,
      reason: "gone",
    });
  });

  it("deletes with the fingerprint the user saw", async () => {
    const result = await store.remove("w-b-site", "fp-1");
    expect(result.ok).toBe(true);
    expect(api.bodies.at(-1)).toEqual({ confirm: true, fingerprint: "fp-1" });
    expect(store.byId("w-b-site")?.busy).toBe("deleting");
  });

  it("passes on a refused delete", async () => {
    api.refuse.set("DELETE /api/workspaces/{id}", {
      status: 409,
      message: "changed since",
    });
    expect(await store.remove("w-b-site", "old")).toMatchObject({
      ok: false,
      reason: "conflict",
      message: "changed since",
    });
  });

  it("attaches in the mode asked", async () => {
    const result = await store.attach("w-a-shop", "desktop");
    expect(result).toMatchObject({ ok: true, value: { opened: true } });
    expect(api.bodies.at(-1)).toEqual({ mode: "desktop" });
  });
});

describe("renaming", () => {
  it("shows the new name in place and sorts by it", async () => {
    await store.refresh();
    const result = await store.rename("w-a-shop", "Zed");
    expect(result.ok).toBe(true);
    expect(api.bodies.at(-1)).toEqual({ name: "Zed" });
    expect(store.list.map((w) => w.name)).toEqual(["b-site", "Zed"]);
    // Looked up by its name: a rename never changes what events and settings call it.
    expect(store.byId("w-a-shop")?.name).toBe("Zed");
  });

  it("a clash is a conflict the form can show, and the list is read again", async () => {
    await store.refresh();
    const result = await store.rename("w-b-site", "A-SHOP");
    expect(result).toMatchObject({ ok: false, reason: "conflict" });
    expect(store.byId("w-b-site")?.name).toBe("b-site");
  });

  it("a workspace deleted meanwhile is gone, and a dead service says so", async () => {
    await store.refresh();
    expect(await store.rename("w-ghost", "x")).toMatchObject({
      ok: false,
      reason: "gone",
    });
    api.down = true;
    expect(await store.rename("w-b-site", "x")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });

  it("the end of an operation carries the name the user gave", async () => {
    await store.refresh();
    await store.rename("w-a-shop", "Shop");
    store.handleEvent(step("w-a-shop", "done"));
    expect(settled).toMatchObject([{ id: "w-a-shop", label: "Shop" }]);
    await store.refresh(); // events wait for a read that is running
    store.handleEvent(step("w-unlisted", "failed", "x"));
    expect(settled.at(-1)).toMatchObject({
      id: "w-unlisted",
      label: "w-unlisted",
    });
  });
});

describe("ids and names", () => {
  it("keys events, progress and the list by id, whatever the name says", async () => {
    await store.refresh();
    await store.rename("w-a-shop", "Shop"); // the name is shown only
    store.handleEvent(status("w-a-shop", "crashed"));
    expect(store.byId("w-a-shop")?.status).toBe("crashed");
    expect(store.byId("w-b-site")?.status).toBe("stopped");
    store.handleEvent(step("w-a-shop", "stopping"));
    expect(Object.keys(store.progress)).toEqual(["w-a-shop"]);
    expect(store.labelOf("w-a-shop")).toBe("Shop");
  });

  it("asks for settings by id", async () => {
    await store.refresh();
    await store.setDirectSsh("w-b-site", true);
    expect(api.calls).toContain("PUT /api/settings/workspaces/{workspace}");
    expect(api.overrides["w-b-site"]?.direct_ssh).toBe(true);
    expect(store.byId("w-b-site")?.direct_ssh).toBe(true);
  });
});

describe("listening", () => {
  it("reads at the start, on a resync and on the slow poll, and applies events", async () => {
    vi.useFakeTimers();
    const stop = store.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(store.status).toBe("ready");
    const reads = () =>
      api.calls.filter((c) => c === "GET /api/workspaces").length;
    expect(reads()).toBe(1);
    source.emit(status("w-b-site", "running"));
    expect(store.byId("w-b-site")?.status).toBe("running");
    source.resync();
    await vi.advanceTimersByTimeAsync(0);
    expect(reads()).toBe(2);
    await vi.advanceTimersByTimeAsync(1000);
    expect(reads()).toBeGreaterThanOrEqual(3);
    stop();
    const after = reads();
    source.emit(status("w-b-site", "crashed"));
    expect(store.byId("w-b-site")?.status).toBe("stopped");
    await vi.advanceTimersByTimeAsync(5000);
    expect(reads()).toBe(after);
  });

  it("stops before the first read finishes", async () => {
    vi.useFakeTimers();
    const stop = store.start();
    stop();
    await vi.advanceTimersByTimeAsync(5000);
    expect(api.calls.filter((c) => c === "GET /api/workspaces")).toHaveLength(
      1,
    );
  });

  it("a poll still in flight when it is stopped schedules nothing more", async () => {
    vi.useFakeTimers();
    const stop = store.start();
    await vi.advanceTimersByTimeAsync(0);
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    const read = api.GET as (...args: unknown[]) => Promise<unknown>;
    api.GET = (async (...args: unknown[]) => {
      await gate;
      return read(...args);
    }) as never;
    await vi.advanceTimersByTimeAsync(1000);
    stop();
    release();
    await vi.advanceTimersByTimeAsync(0);
    const reads = api.calls.length;
    await vi.advanceTimersByTimeAsync(120_000);
    expect(api.calls.length).toBe(reads);
  });
});

describe("fieldOf", () => {
  it.each([
    ["use an https:// URL for the repository", "repo_url"],
    ["SSH remotes are not supported yet", "repo_url"],
    ["bad image reference", "image"],
    ["memory must be at least 256 MiB", "memory_mib"],
    ["this name can't be a workspace: x", "name"],
    ["a workspace named x already exists", "name"],
    ["the start folder can't be inside .puddle", "start_folder"],
    ["repositories 1 (a) and 2 (b) both go into api", "repo_url"],
    ["something else", "form"],
  ])("%j goes to %s", (message, field) => {
    expect(fieldOf(message)).toBe(field);
  });
});
