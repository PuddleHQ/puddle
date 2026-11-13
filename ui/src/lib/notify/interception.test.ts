// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import type { NoticeInput } from "./notices.svelte.ts";
import {
  InterceptionChoiceNotice,
  INTERCEPTION_NOTICE_KEY,
} from "./interception.ts";
import type { Notifier } from "./notifier.ts";

function setup(
  view: object | undefined,
  options: { throws?: boolean; putFails?: boolean; path?: string } = {},
) {
  const raised: NoticeInput[] = [];
  const resolved: string[] = [];
  const notifier: Notifier = {
    notify: (n) => void raised.push(n),
    resolve: (key) => void resolved.push(key),
  };
  const puts: unknown[] = [];
  const api = {
    GET: async () => {
      if (options.throws) throw new TypeError("down");
      return { data: view };
    },
    PUT: async (_path: string, init: { body: unknown }) => {
      puts.push(init.body);
      return { data: options.putFails ? undefined : view };
    },
  };
  return {
    raised,
    resolved,
    puts,
    notice: new InterceptionChoiceNotice(
      notifier,
      api as never,
      () => options.path ?? "/workspaces",
    ),
  };
}

const open = { chosen: false, notice_seen: false };

describe("the interception choice notice", () => {
  it("is raised for an install that has neither chosen nor seen it", async () => {
    const t = setup(open);
    await t.notice.refresh();
    expect(t.raised).toHaveLength(1);
    expect(t.raised[0]).toMatchObject({
      key: INTERCEPTION_NOTICE_KEY,
      tone: "info",
      link: { href: "/settings#s-net" },
    });
  });

  it("stays away once a mode was chosen, once it was seen, or when the answer is missing", async () => {
    for (const view of [
      { chosen: true, notice_seen: true },
      { chosen: false, notice_seen: true },
      undefined,
    ]) {
      const t = setup(view);
      await t.notice.refresh();
      expect(t.raised).toEqual([]);
    }
    const down = setup(open, { throws: true });
    await down.notice.refresh();
    expect(down.raised).toEqual([]);
  });

  it("stays away during the first-run flow, which asks the question itself", async () => {
    const t = setup(open, { path: "/welcome/connect" });
    await t.notice.refresh();
    expect(t.raised).toEqual([]);
  });

  it("keeping it answers the notice and removes it", async () => {
    const t = setup(open);
    await t.notice.refresh();
    await t.raised[0]?.action?.run();
    expect(t.puts).toEqual([{ notice_seen: true }]);
    expect(t.resolved).toEqual([INTERCEPTION_NOTICE_KEY]);
  });

  it("keeps the notice and says so when the answer could not be saved", async () => {
    const t = setup(open, { putFails: true });
    await t.notice.refresh();
    await expect(t.raised[0]?.action?.run()).rejects.toThrow(/couldn't keep/);
    expect(t.resolved).toEqual([]);
  });
});
