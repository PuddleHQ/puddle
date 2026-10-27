// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { EnvironmentNotices } from "./environment.ts";
import { InAppNotifier } from "./notifier.ts";
import { NoticeCenter } from "./notices.svelte.ts";
import { NoticeWatcher } from "./watcher.ts";

let center: NoticeCenter;
let notices: EnvironmentNotices;

const envFailed = {
  type: "workspace_env_failed",
  workspace: "web",
  reason: "the credential store is locked",
};
const portFailed = {
  type: "workspace_port_forward_failed",
  workspace: "web",
  guest_port: 3000,
  host_port: 3001,
  reason: "port 3001 on this computer is in use by node; choose another port",
};

beforeEach(() => {
  center = new NoticeCenter();
  notices = new EnvironmentNotices(new InAppNotifier(center));
});

describe("a change that did not reach a running workspace", () => {
  it("says why and links to the environment tab", () => {
    expect(notices.handle(envFailed)).toBe(true);
    expect(center.items).toHaveLength(1);
    expect(center.items[0]).toMatchObject({
      key: "env:web",
      tone: "warning",
      title: "A change to the environment of web did not reach it.",
      detail: "the credential store is locked",
      link: { href: "/workspaces/web/environment" },
    });
  });

  it("replaces the notice for the same workspace instead of piling up", () => {
    notices.handle(envFailed);
    notices.handle({ ...envFailed, reason: "again" });
    expect(center.items).toHaveLength(1);
    expect(center.items[0]?.detail).toBe("again");
  });
});

describe("a kept forward that could not be opened", () => {
  it("names both ports, says why and links to the ports tab", () => {
    expect(notices.handle(portFailed)).toBe(true);
    expect(center.items[0]).toMatchObject({
      key: "port:web:3000",
      tone: "warning",
      title: "Port 3000 of web could not be forwarded to 127.0.0.1:3001.",
      link: { href: "/workspaces/web/ports" },
    });
    expect(center.items[0]?.detail).toContain("in use by node");
  });
});

describe("events that are neither", () => {
  it.each([
    null,
    "text",
    { type: "oom_kill" },
    { ...envFailed, reason: 1 },
    { ...portFailed, host_port: "x" },
  ])("are ignored (%j)", (raw) => {
    expect(notices.handle(raw)).toBe(false);
    expect(center.items).toHaveLength(0);
  });
});

describe("the notice watcher", () => {
  it("turns both events from the stream into notices", () => {
    const watcher = new NoticeWatcher({ notifier: new InAppNotifier(center) });
    watcher.handle(envFailed);
    watcher.handle(portFailed);
    expect(center.items.map((n) => n.key)).toEqual([
      "env:web",
      "port:web:3000",
    ]);
  });
});
