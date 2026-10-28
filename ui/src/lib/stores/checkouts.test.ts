// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import {
  FakeWorkspaces,
  checkout,
  listing,
  workspace,
} from "#lib/testing/fake-workspaces.ts";
import { CheckoutsStore, touchesCheckouts } from "./checkouts.svelte.ts";

let api: FakeWorkspaces;
let source: FakeSource;
let refreshed: number;
let store: CheckoutsStore;

beforeEach(() => {
  api = new FakeWorkspaces();
  api.list = [workspace("dev", { status: "running" })];
  source = new FakeSource();
  refreshed = 0;
  store = new CheckoutsStore(api as never, source, async () => {
    refreshed += 1;
  });
});

describe("the checkout listing", () => {
  it("reads the listing and keeps it", async () => {
    api.checkouts["dev"] = listing([checkout("api")]);
    await store.load("dev");
    expect(store.status).toBe("ready");
    expect(store.listing?.checkouts.map((c) => c.directory)).toEqual(["api"]);
  });

  it("says why it could not be read when there is nothing to show", async () => {
    api.refuse.set("GET /api/workspaces/{id}/checkouts", {
      status: 404,
      message: "no such workspace",
    });
    await store.load("dev");
    expect(store.status).toBe("failed");
    expect(store.error).toBe("No such workspace.");
    api.down = true;
    await store.load("dev");
    expect(store.error).toBe("Puddle's service isn't answering.");
  });

  it("keeps the last listing when a later read fails", async () => {
    api.checkouts["dev"] = listing([checkout("api")]);
    await store.load("dev");
    api.down = true;
    await store.load("dev", true);
    expect(store.status).toBe("ready");
    expect(store.listing?.checkouts).toHaveLength(1);
    expect(store.error).toBe("Puddle's service isn't answering.");
    api.down = false;
    api.refuse.set("GET /api/workspaces/{id}/checkouts", {
      status: 409,
      message: "busy",
    });
    await store.load("dev", true);
    expect(store.status).toBe("ready");
    expect(store.error).toBe("Busy.");
  });

  it("forgets the listing of another workspace", async () => {
    api.list.push(workspace("other", { status: "running" }));
    api.checkouts["dev"] = listing([checkout("api")]);
    await store.load("dev");
    api.checkouts["other"] = listing([]);
    await store.load("other");
    expect(store.listing?.checkouts).toEqual([]);
  });

  it("never lets a slow answer overwrite a newer one", async () => {
    api.checkouts["dev"] = listing([checkout("old")]);
    const first = store.load("dev");
    api.checkouts["dev"] = listing([checkout("new")]);
    await store.load("dev", true);
    await first;
    expect(store.listing?.checkouts[0]?.directory).toBe("new");
  });

  it("never lets a slow failure replace a newer answer", async () => {
    api.checkouts["dev"] = listing([checkout("new")]);
    const read = api.GET;
    let fail!: () => void;
    const gate = new Promise<void>(
      (_, reject) => (fail = () => reject(new TypeError("down"))),
    );
    api.GET = (async (...args: Parameters<typeof read>) => {
      api.GET = read;
      await gate;
      return read(...args);
    }) as never;
    const first = store.load("dev");
    await store.load("dev", true);
    fail();
    await first;
    expect(store.status).toBe("ready");
    expect(store.error).toBeNull();
    expect(store.listing?.checkouts[0]?.directory).toBe("new");
  });

  it("is read again when the workspace's list, state or operations change", async () => {
    const stop = store.start("dev");
    await vi.waitFor(() => expect(store.status).toBe("ready"));
    const reads = () =>
      api.calls.filter((c) => c === "GET /api/workspaces/{id}/checkouts")
        .length;
    const before = reads();
    source.emit({ type: "workspace_repositories_changed", workspace: "dev" });
    await vi.waitFor(() => expect(reads()).toBe(before + 1));
    source.resync();
    await vi.waitFor(() => expect(reads()).toBe(before + 2));
    stop();
    source.emit({ type: "workspace_repositories_changed", workspace: "dev" });
    expect(reads()).toBe(before + 2);
  });

  it("knows which events can change it", () => {
    const event = (e: Record<string, unknown>) =>
      touchesCheckouts({ workspace: "dev", ...e }, "dev");
    expect(event({ type: "workspace_repositories_changed" })).toBe(true);
    expect(event({ type: "status_changed", status: "running" })).toBe(true);
    expect(event({ type: "workspace_progress", step: "done" })).toBe(true);
    expect(event({ type: "workspace_progress", step: "failed" })).toBe(true);
    expect(event({ type: "workspace_progress", step: "cloning" })).toBe(false);
    expect(event({ type: "oom_kill" })).toBe(false);
    expect(
      touchesCheckouts({ type: "status_changed", workspace: "other" }, "dev"),
    ).toBe(false);
    expect(touchesCheckouts("text", "dev")).toBe(false);
    expect(touchesCheckouts(null, "dev")).toBe(false);
  });
});

describe("changing the list", () => {
  beforeEach(async () => {
    await store.load("dev");
  });

  it("adds a clone and reads the workspace and the listing again", async () => {
    const result = await store.add({
      kind: "clone",
      url: "https://github.com/acme/web.git",
      directory: "web",
    });
    expect(result).toEqual({ ok: true });
    expect(api.bodies.at(-1)).toMatchObject({
      kind: "clone",
      directory: "web",
    });
    expect(refreshed).toBe(1);
  });

  it("carries the address the user saw with a manage request", async () => {
    await store.add({
      kind: "existing",
      directory: "scratch",
      url: "https://github.com/acme/scratch.git",
    });
    expect(api.bodies.at(-1)).toEqual({
      kind: "existing",
      directory: "scratch",
      url: "https://github.com/acme/scratch.git",
    });
  });

  it("clones an entry again and takes one off the list", async () => {
    expect(await store.cloneAgain("dev")).toEqual({ ok: true });
    expect(api.calls).toContain(
      "POST /api/workspaces/{id}/repositories/{directory}/clone",
    );
    expect(await store.remove("dev")).toEqual({ ok: true });
    expect(api.list[0]?.repositories).toEqual([]);
    expect(refreshed).toBe(2);
  });

  it("returns a refusal as a sentence and changes nothing", async () => {
    api.refuse.set("POST /api/workspaces/{id}/repositories", {
      status: 409,
      message: "the remote of scratch is now https://x/y",
    });
    const result = await store.add({
      kind: "existing",
      directory: "scratch",
      url: "https://github.com/acme/scratch.git",
    });
    expect(result).toEqual({
      ok: false,
      message: "The remote of scratch is now https://x/y.",
    });
    expect(refreshed).toBe(0);
  });

  it("says when the service does not answer", async () => {
    api.down = true;
    expect(await store.remove("dev")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });
});
