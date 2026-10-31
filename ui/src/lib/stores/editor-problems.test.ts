// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import { EditorProblemsStore } from "./editor-problems.svelte.ts";

const issue = (workspace: string, reason: string) => ({
  key: `editor-install:${workspace}`,
  workspace,
  title: `The browser editor could not be installed in ${workspace}: ${reason}`,
  detail: "d",
});

let listed: ReturnType<typeof issue>[];
let down: boolean;
let source: FakeSource;
const api = {
  GET: async () => {
    if (down) throw new Error("down");
    return { data: { problems: listed } };
  },
};
const make = () => new EditorProblemsStore(api as never, source);

beforeEach(() => {
  listed = [];
  down = false;
  source = new FakeSource();
});

describe("the editor update problem", () => {
  it("names the update problem of one workspace apart from its install problem", async () => {
    listed = [
      issue("web", "bad checksum"),
      {
        key: "editor-update:web",
        workspace: "web",
        title:
          "The browser editor in web could not update: a newer build was refused",
        detail: "d",
      },
    ];
    const store = make();
    await store.load();
    expect(store.updateOf("web")?.title).toMatch(
      /could not update: a newer build was refused$/,
    );
    expect(store.of("web")?.title).toMatch(/installed in web/);
    expect(store.updateOf("docs")).toBeNull();
  });
});

describe("the editor install problem", () => {
  it("names the problem of one workspace and none for another", async () => {
    listed = [issue("web", "bad checksum")];
    const store = make();
    await store.load();
    expect(store.of("web")?.title).toMatch(/installed in web: bad checksum$/);
    expect(store.of("docs")).toBeNull();
  });

  it("reads again when the host says its problems changed, and on a resync", async () => {
    const store = make();
    const stop = store.start();
    await vi.waitFor(() => expect(store.problems).toEqual([]));
    listed = [issue("web", "bad checksum")];
    source.emit({ type: "status_changed" });
    await Promise.resolve();
    expect(store.of("web")).toBeNull();
    source.emit({ type: "problems_changed" });
    await vi.waitFor(() => expect(store.of("web")).not.toBeNull());
    listed = [];
    source.resync();
    await vi.waitFor(() => expect(store.of("web")).toBeNull());
    stop();
    listed = [issue("web", "again")];
    source.emit({ type: "problems_changed" });
    await Promise.resolve();
    expect(store.of("web")).toBeNull();
  });

  it("keeps what it shows when a read fails", async () => {
    listed = [issue("web", "bad checksum")];
    const store = make();
    await store.load();
    down = true;
    await store.load();
    expect(store.of("web")).not.toBeNull();
  });

  it("never lets a slow answer overwrite a newer one", async () => {
    const answers: ((problems: ReturnType<typeof issue>[]) => void)[] = [];
    const slow = {
      GET: () =>
        new Promise((resolve) => {
          answers.push((problems) => resolve({ data: { problems } }));
        }),
    };
    const store = new EditorProblemsStore(slow as never, source);
    const first = store.load();
    const second = store.load();
    answers[1]?.([issue("new", "y")]);
    await second;
    answers[0]?.([issue("old", "x")]);
    await first;
    expect(store.of("new")).not.toBeNull();
    expect(store.of("old")).toBeNull();
  });
});
