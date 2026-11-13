// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { DiskSpaceNotices } from "./disk-space.ts";
import { InAppNotifier } from "./notifier.ts";
import { NoticeCenter } from "./notices.svelte.ts";
import { NoticeWatcher } from "./watcher.ts";

let center: NoticeCenter;
let wanted: boolean;
let notices: DiskSpaceNotices;

const changed = (level: string, free_mib: number) => ({
  type: "disk_space_changed",
  level,
  free_mib,
});

beforeEach(() => {
  center = new NoticeCenter();
  wanted = true;
  notices = new DiskSpaceNotices(new InAppNotifier(center), () => wanted);
});

describe("free space on the drive that holds Puddle's data", () => {
  it("says it is running low, and how much is left", () => {
    expect(notices.handle(changed("low", 15_360))).toBe(true);
    expect(center.items).toHaveLength(1);
    expect(center.items[0]).toMatchObject({
      key: "disk-space",
      tone: "warning",
      title: "Disk space is running low.",
      link: { href: "/settings#s-disk", label: "Disk space settings" },
    });
    expect(center.items[0]!.detail).toMatch(/^15 GB are left on the drive/);
  });

  it("replaces the notice when it gets worse and withdraws it when the space is back", () => {
    notices.handle(changed("low", 15_360));
    notices.handle(changed("critical", 3_584));
    expect(center.items).toHaveLength(1);
    expect(center.items[0]).toMatchObject({
      title: "Disk space is almost gone.",
    });
    expect(center.items[0]!.detail).toMatch(/^3\.5 GB are left/);
    expect(notices.handle(changed("ok", 40_960))).toBe(true);
    expect(center.items).toHaveLength(0);
  });

  it("stays silent while the user turned the notice off, and still withdraws one", () => {
    wanted = false;
    expect(notices.handle(changed("low", 15_360))).toBe(true);
    expect(center.items).toHaveLength(0);
    wanted = true;
    notices.handle(changed("low", 15_360));
    wanted = false;
    notices.handle(changed("ok", 40_960));
    expect(center.items).toHaveLength(0);
  });

  it("ignores other events", () => {
    expect(notices.handle({ type: "oom_kill" })).toBe(false);
    expect(notices.handle(null)).toBe(false);
    expect(center.items).toHaveLength(0);
  });

  it("is part of the watcher that listens to the event stream", () => {
    const watcher = new NoticeWatcher({
      notifier: new InAppNotifier(center),
      lowDiskSpaceNotice: () => true,
    });
    watcher.handle(changed("critical", 2_048));
    expect(center.items.map((n) => n.key)).toEqual(["disk-space"]);
  });

  it("is wanted by default when the stored settings are not loaded yet", () => {
    const watcher = new NoticeWatcher({ notifier: new InAppNotifier(center) });
    watcher.handle(changed("low", 15_360));
    expect(center.items.map((n) => n.key)).toEqual(["disk-space"]);
  });
});
