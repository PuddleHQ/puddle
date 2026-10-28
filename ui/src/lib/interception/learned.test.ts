// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { NoticeWatcher } from "#lib/notify/watcher.ts";
import { InAppNotifier } from "#lib/notify/notifier.ts";
import { NoticeCenter } from "#lib/notify/notices.svelte.ts";
import {
  asPassThroughLearned,
  decryptAgain,
  LearnedNotices,
  timeLeft,
} from "./learned.ts";

const event = {
  type: "pass_through_learned",
  workspace: "web",
  host: "pinned.example",
};

describe("asPassThroughLearned", () => {
  it("accepts the event and ignores anything else", () => {
    expect(asPassThroughLearned(event)).toEqual(event);
    expect(asPassThroughLearned({ ...event, host: 3 })).toBeNull();
    expect(asPassThroughLearned({ ...event, workspace: undefined })).toBeNull();
    expect(asPassThroughLearned({ ...event, type: "other" })).toBeNull();
    expect(asPassThroughLearned(null)).toBeNull();
    expect(asPassThroughLearned("text")).toBeNull();
  });
});

describe("timeLeft", () => {
  it("says it in the biggest sensible unit", () => {
    expect(timeLeft(30)).toBe("less than a minute");
    expect(timeLeft(60)).toBe("1 minute");
    expect(timeLeft(300)).toBe("5 minutes");
    expect(timeLeft(3600)).toBe("1 hour");
    expect(timeLeft(43_200)).toBe("12 hours");
    expect(timeLeft(5400)).toBe("1 hour 30 minutes");
    expect(timeLeft(7260)).toBe("2 hours 1 minute");
  });
});

class Stub {
  calls: string[] = [];
  status = 204;
  down = false;
  message = "";

  async DELETE(
    path: string,
    init: { params: { path: Record<string, string> } },
  ) {
    this.calls.push(`DELETE ${path} ${JSON.stringify(init.params.path)}`);
    if (this.down) throw new TypeError("down");
    return {
      response: { status: this.status, ok: this.status < 400 },
      error: this.message ? { message: this.message } : undefined,
    };
  }
}

describe("decryptAgain", () => {
  it("asks the API to forget the host and counts a host already gone as done", async () => {
    const api = new Stub();
    await decryptAgain(api as never, "web", "pinned.example");
    api.status = 404;
    await decryptAgain(api as never, "web", "pinned.example");
    expect(api.calls).toEqual([
      'DELETE /api/workspaces/{id}/interception/learned/{host} {"id":"web","host":"pinned.example"}',
      'DELETE /api/workspaces/{id}/interception/learned/{host} {"id":"web","host":"pinned.example"}',
    ]);
  });

  it("throws the API's own words, or says the service is down", async () => {
    const api = new Stub();
    api.status = 500;
    api.message = "the list is locked";
    await expect(
      decryptAgain(api as never, "web", "a.example"),
    ).rejects.toThrow("The list is locked.");
    api.message = "";
    await expect(
      decryptAgain(api as never, "web", "a.example"),
    ).rejects.toThrow("Puddle couldn't decrypt the host again.");
    api.down = true;
    await expect(
      decryptAgain(api as never, "web", "a.example"),
    ).rejects.toThrow("Puddle's service isn't answering. Try again.");
  });
});

describe("the notice for a host Puddle stopped decrypting", () => {
  let center: NoticeCenter;
  let api: Stub;
  let notices: LearnedNotices;

  beforeEach(() => {
    center = new NoticeCenter();
    api = new Stub();
    notices = new LearnedNotices(new InAppNotifier(center), api as never);
  });

  it("names the host and why, links to the workspace settings and offers one action", () => {
    expect(notices.handle(event)).toBe(true);
    expect(center.items).toHaveLength(1);
    const notice = center.items[0]!;
    expect(notice).toMatchObject({
      key: "learned:web:pinned.example",
      tone: "info",
      title:
        "Puddle stopped decrypting pinned.example: its client refused Puddle's certificate.",
      link: { href: "/workspaces/web/settings", label: "Workspace settings" },
      action: { label: "Decrypt again" },
    });
    expect(notice.detail).toContain("web");
    expect(notice.detail).toContain("own list of trusted roots");
    expect(notice.action?.confirm).toBeUndefined();
  });

  it("ends the notice when the host is decrypted again, and keeps it when that fails", async () => {
    notices.handle(event);
    api.status = 500;
    await expect(center.items[0]!.action!.run()).rejects.toThrow();
    expect(center.items).toHaveLength(1);
    api.status = 204;
    await center.items[0]!.action!.run();
    expect(center.items).toHaveLength(0);
  });

  it("is one notice per host and workspace, and ignores other events", () => {
    notices.handle(event);
    notices.handle(event);
    notices.handle({ ...event, host: "other.example" });
    expect(center.items).toHaveLength(2);
    expect(notices.handle({ type: "rules_changed" })).toBe(false);
  });
});

describe("the notice watcher", () => {
  it("raises the notice for the event on the stream", () => {
    const center = new NoticeCenter();
    new NoticeWatcher({ notifier: new InAppNotifier(center) }).handle(event);
    expect(center.items.map((n) => n.key)).toEqual([
      "learned:web:pinned.example",
    ]);
  });
});
