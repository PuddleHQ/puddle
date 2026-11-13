// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import type { LiveListener } from "./live.svelte.ts";
import { OrphanVolumes } from "./orphan-volumes.svelte.ts";

type Reply = { data?: unknown; error?: { message: string }; status?: number };

function scripted(get: Reply[], post: Reply[] = []) {
  const reply = (r: Reply | undefined) => {
    const status = r?.status ?? (r?.error ? 500 : 200);
    return { data: r?.data, error: r?.error, response: { status } };
  };
  return {
    GET: vi.fn(async () => reply(get.shift())),
    POST: vi.fn(async () => reply(post.shift())),
  };
}

const lost = { id: "lost", volume: "ws-lost" };
const other = { id: "other", volume: "ws-other" };

describe("orphan volumes", () => {
  it("reads the disks the list does not name", async () => {
    const api = scripted([{ data: { volumes: [lost, other] } }]);
    const store = new OrphanVolumes(api as never, undefined);
    await store.load();
    expect(store.volumes.map((v) => v.id)).toEqual(["lost", "other"]);
    expect(api.GET).toHaveBeenCalledWith("/api/orphan-volumes");
  });

  it("keeps what is shown when a read fails", async () => {
    const store = new OrphanVolumes(
      scripted([
        { data: { volumes: [lost] } },
        { error: { message: "x" } },
      ]) as never,
      undefined,
    );
    await store.load();
    await store.load();
    expect(store.volumes).toEqual([lost]);
  });

  it("restores a disk, sends the address only when given, and drops the disk from the list", async () => {
    const api = scripted(
      [{ data: { volumes: [lost, other] } }],
      [{ status: 201, data: { name: "lost" } }],
    );
    const store = new OrphanVolumes(api as never, undefined);
    await store.load();
    const result = await store.restore("lost", "  ");
    expect(result).toEqual({ ok: true, name: "lost" });
    expect(api.POST).toHaveBeenCalledWith("/api/orphan-volumes/{id}/restore", {
      params: { path: { id: "lost" } },
      body: {},
    });
    expect(store.volumes).toEqual([other]);
  });

  it("asks for an address when the disk gave none, and keeps the disk when a restore fails", async () => {
    const api = scripted(
      [{ data: { volumes: [lost] } }],
      [
        {
          status: 422,
          error: {
            message: "Puddle could not read a repository address from ws-lost",
          },
        },
        {
          status: 409,
          error: { message: "a workspace named lost is already in the list" },
        },
        { status: 201, data: { name: "lost" } },
      ],
    );
    const store = new OrphanVolumes(api as never, undefined);
    await store.load();
    const asked = await store.restore("lost");
    expect(asked).toMatchObject({ ok: false, needsAddress: true });
    expect(store.volumes).toEqual([lost]);
    const clash = await store.restore("lost", "https://example.org/a/b");
    expect(clash).toMatchObject({ ok: false, needsAddress: false });
    expect(api.POST).toHaveBeenLastCalledWith(
      "/api/orphan-volumes/{id}/restore",
      {
        params: { path: { id: "lost" } },
        body: { repository: "https://example.org/a/b" },
      },
    );
    expect(await store.restore("lost", "https://example.org/a/b")).toEqual({
      ok: true,
      name: "lost",
    });
  });

  it("says why when the service does not answer", async () => {
    const api = {
      GET: vi.fn(),
      POST: vi.fn(async () => {
        throw new Error("network down");
      }),
    };
    const result = await new OrphanVolumes(api as never, undefined).restore(
      "lost",
    );
    expect(result).toEqual({
      ok: false,
      message: "network down",
      needsAddress: false,
    });
  });

  it("rereads on a problems or status change and on a resync, and stops when told", async () => {
    let listener: LiveListener = {};
    const unsubscribe = vi.fn();
    const source = {
      subscribe: (l: LiveListener) => {
        listener = l;
        return unsubscribe;
      },
    };
    const api = scripted([
      { data: { volumes: [lost] } },
      { data: { volumes: [] } },
      { data: { volumes: [other] } },
      { data: { volumes: [] } },
    ]);
    const store = new OrphanVolumes(api as never, source);
    const stop = store.start();
    await vi.waitFor(() => expect(store.volumes).toEqual([lost]));
    listener.event?.({ type: "problems_changed" });
    await vi.waitFor(() => expect(store.volumes).toEqual([]));
    listener.event?.({ type: "something_else" });
    listener.event?.({ type: "status_changed" });
    await vi.waitFor(() => expect(store.volumes).toEqual([other]));
    listener.resync?.();
    await vi.waitFor(() => expect(api.GET).toHaveBeenCalledTimes(4));
    stop();
    expect(unsubscribe).toHaveBeenCalled();
  });
});
