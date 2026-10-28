// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  GET: vi.fn(),
  POST: vi.fn(),
  PUT: vi.fn(),
  DELETE: vi.fn(),
}));
vi.mock("#lib/api/client.ts", () => ({ api }));

import { FakeInbox, request } from "#lib/testing/fake-inbox.ts";
import { mine } from "#lib/testing/fake-rule-sets.ts";
import { live } from "./live.svelte.ts";
import { pending } from "./pending.svelte.ts";
import { ruleSets } from "./rule-sets.svelte.ts";
import { rulesStore } from "./rules.svelte.ts";

beforeEach(() => {
  vi.restoreAllMocks();
  api.GET.mockReset();
  api.POST.mockReset();
  api.PUT.mockReset();
});

describe("the shared stores and the nav badge", () => {
  it("the inbox store shows its open-request count on the badge", async () => {
    const inbox = new FakeInbox();
    inbox.add(request(1));
    inbox.add(request(2));
    api.GET.mockImplementation(inbox.GET);
    await pending.refresh();
    expect(live.pending).toBe(2);
  });

  it("the rules store refreshes the badge after a rule is added", async () => {
    const refresh = vi.spyOn(live, "refresh").mockResolvedValue();
    api.POST.mockResolvedValue({
      data: { id: 7 },
      response: { ok: true, status: 201 },
    });
    const result = await rulesStore.add({
      pattern: ".example.com",
      effect: "allow",
      scope: { type: "global" },
    } as never);
    expect(result.ok).toBe(true);
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("the rule sets store refreshes the badge after a switch closed requests", async () => {
    const refresh = vi.spyOn(live, "refresh").mockResolvedValue();
    api.PUT.mockResolvedValue({
      data: { set: mine(1), closed: [3] },
      response: { ok: true, status: 200 },
    });
    expect(await ruleSets.switchSet("user:1", null, true)).toEqual({
      ok: true,
      value: 1,
    });
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});
