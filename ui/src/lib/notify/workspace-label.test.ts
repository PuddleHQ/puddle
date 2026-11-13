// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, describe, expect, it } from "vitest";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import { workspace } from "#lib/testing/fake-workspaces.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { InAppNotifier } from "./notifier.ts";
import { NoticeCenter } from "./notices.svelte.ts";
import { NoticeWatcher } from "./watcher.ts";

afterEach(() => {
  workspaces.list = [];
});

describe("notices name a workspace by its display name and key it by its id", () => {
  it("shows the name in the words, the id in the key and the link", () => {
    workspaces.list = [workspace("web", { name: "Web shop" })];
    const center = new NoticeCenter();
    const watcher = new NoticeWatcher({
      source: new FakeSource(),
      notifier: new InAppNotifier(center),
      api: { GET: async () => ({ response: { status: 500 } }) } as never,
    });
    watcher.handle({
      type: "status_changed",
      workspace: "w-web",
      status: "running",
    });
    watcher.handle({
      type: "status_changed",
      workspace: "w-web",
      status: "crashed",
    });
    const notice = center.items[0];
    expect(notice?.key).toBe("stop:w-web");
    expect(notice?.title).toBe("Web shop crashed.");
    expect(notice?.link?.href).toBe("/workspaces/w-web");
  });

  it("falls back to the id for a workspace that is no longer listed", () => {
    expect(workspaces.labelOf("w-gone")).toBe("w-gone");
    workspaces.list = [workspace("web", { name: "Web shop" })];
    expect(workspaces.labelOf("w-web")).toBe("Web shop");
  });
});
