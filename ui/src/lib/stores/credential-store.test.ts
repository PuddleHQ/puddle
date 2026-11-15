// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import type { LiveListener } from "./live.svelte.ts";
import { CredentialStoreState } from "./credential-store.svelte.ts";

const problem = { key: "credential-store", title: "t", detail: "d" };

describe("the credential store state", () => {
  it("rereads the problem list on problems_changed and on a resync, and stops when told", async () => {
    let listener: LiveListener | undefined;
    const unsubscribe = vi.fn();
    let list: unknown[] = [];
    const api = {
      GET: vi.fn(async () => ({ data: { problems: list } })),
      POST: vi.fn(),
    };
    const store = new CredentialStoreState(api as never, {
      subscribe: (l) => {
        listener = l;
        return unsubscribe;
      },
    });
    const stop = store.start();
    await vi.waitFor(() => expect(api.GET).toHaveBeenCalledTimes(1));
    expect(store.problem).toBeNull();

    list = [problem];
    listener?.event?.({ type: "problems_changed" });
    await vi.waitFor(() => expect(store.problem?.key).toBe("credential-store"));

    list = [];
    listener?.event?.({ type: "something_else" });
    listener?.resync?.();
    await vi.waitFor(() => expect(store.problem).toBeNull());

    stop();
    expect(unsubscribe).toHaveBeenCalled();
  });

  it("keeps what it shows when a read fails", async () => {
    const api = {
      GET: vi
        .fn()
        .mockResolvedValueOnce({ data: { problems: [problem] } })
        .mockRejectedValueOnce(new Error("down")),
      POST: vi.fn(),
    };
    const store = new CredentialStoreState(api as never, undefined);
    await store.load();
    await store.load();
    expect(store.problem?.key).toBe("credential-store");
  });

  it("says Puddle could not be reached when the retry request fails, and takes one retry at a time", async () => {
    const api = {
      GET: vi.fn(async () => ({ data: { problems: [problem] } })),
      POST: vi.fn().mockRejectedValue(new Error("down")),
    };
    const store = new CredentialStoreState(api as never, undefined);
    const first = store.retry();
    await store.retry();
    await first;
    expect(api.POST).toHaveBeenCalledTimes(1);
    expect(store.retryError).toBe("Puddle could not be reached.");
    expect(store.retrying).toBe(false);
  });
});
