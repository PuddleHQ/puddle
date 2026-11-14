// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { NoticeWatcher } from "#lib/notify/watcher.ts";
import { InAppNotifier } from "#lib/notify/notifier.ts";
import { NoticeCenter } from "#lib/notify/notices.svelte.ts";
import {
  asPassThroughLearned,
  decryptAgain,
  howLongAgo,
  LearnedNotices,
  makePermanent,
  NOTICE_FROM_COUNT,
  onLearnedChanged,
  refusals,
  timeLeft,
} from "./learned.ts";

const event = {
  type: "pass_through_learned",
  workspace: "web",
  host: "pinned.example",
  count: 3,
};

describe("asPassThroughLearned", () => {
  it("accepts the event and ignores anything else", () => {
    expect(asPassThroughLearned(event)).toEqual(event);
    expect(asPassThroughLearned({ ...event, host: 3 })).toBeNull();
    expect(asPassThroughLearned({ ...event, count: "3" })).toBeNull();
    expect(asPassThroughLearned({ ...event, count: undefined })).toBeNull();
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

describe("howLongAgo", () => {
  const now = 1_000_000_000_000;
  it("says it in the biggest sensible unit and never in the future", () => {
    expect(howLongAgo(now - 30_000, now)).toBe("just now");
    expect(howLongAgo(now + 5_000, now)).toBe("just now");
    expect(howLongAgo(now - 60_000, now)).toBe("1 minute ago");
    expect(howLongAgo(now - 59 * 60_000, now)).toBe("59 minutes ago");
    expect(howLongAgo(now - 60 * 60_000, now)).toBe("1 hour ago");
    expect(howLongAgo(now - 47 * 3_600_000, now)).toBe("47 hours ago");
    expect(howLongAgo(now - 48 * 3_600_000, now)).toBe("2 days ago");
    expect(howLongAgo(now - 100 * 86_400_000, now)).toBe("100 days ago");
  });

  it("reads the clock when none is given", () => {
    expect(howLongAgo(Date.now() - 2 * 3_600_000)).toBe("2 hours ago");
  });
});

describe("refusals", () => {
  it("counts them", () => {
    expect(refusals(1)).toBe("refused once");
    expect(refusals(4)).toBe("refused 4 times");
  });
});

class Stub {
  calls: string[] = [];
  status = 204;
  down = false;
  message = "";

  bodies: unknown[] = [];

  async POST(
    path: string,
    init: { params: { path: Record<string, string> }; body: unknown },
  ) {
    this.calls.push(`POST ${path} ${JSON.stringify(init.params.path)}`);
    this.bodies.push(init.body);
    return this.DELETE(path, init);
  }

  async DELETE(
    path: string,
    init: { params: { path: Record<string, string> } },
  ) {
    if (!path.endsWith("/permanent"))
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

  it("tells listeners which workspace a host was decrypted again in, not after a failure or once removed", async () => {
    const api = new Stub();
    const heard: string[] = [];
    const stop = onLearnedChanged((workspace) => heard.push(workspace));
    await decryptAgain(api as never, "web", "a.example");
    api.status = 500;
    await expect(
      decryptAgain(api as never, "web", "b.example"),
    ).rejects.toThrow();
    stop();
    api.status = 200;
    await decryptAgain(api as never, "other", "c.example");
    expect(heard).toEqual(["web"]);
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

describe("makePermanent", () => {
  it("asks the API to put the host on the never-decrypt names of the scope", async () => {
    const api = new Stub();
    await makePermanent(api as never, "web", "pinned.example", "global");
    expect(api.calls).toEqual([
      'POST /api/workspaces/{id}/interception/learned/{host}/permanent {"id":"web","host":"pinned.example"}',
    ]);
    expect(api.bodies).toEqual([{ scope: "global" }]);
  });

  it("tells the listeners of the workspace, not after a failure", async () => {
    const api = new Stub();
    const heard: string[] = [];
    const stop = onLearnedChanged((workspace) => heard.push(workspace));
    await makePermanent(api as never, "web", "a.example", "workspace");
    api.status = 500;
    await expect(
      makePermanent(api as never, "other", "b.example", "workspace"),
    ).rejects.toThrow();
    stop();
    expect(heard).toEqual(["web"]);
  });

  it("throws the API's own words, or says the service is down", async () => {
    const api = new Stub();
    api.status = 422;
    api.message = "the list is full";
    await expect(
      makePermanent(api as never, "web", "a.example", "workspace"),
    ).rejects.toThrow("The list is full.");
    api.message = "";
    await expect(
      makePermanent(api as never, "web", "a.example", "workspace"),
    ).rejects.toThrow("Puddle couldn't make the host permanent.");
    api.down = true;
    await expect(
      makePermanent(api as never, "web", "a.example", "workspace"),
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

  it("waits for the third refusal, then names the host and why and suggests making it permanent", () => {
    expect(NOTICE_FROM_COUNT).toBe(3);
    for (const count of [1, 2]) {
      expect(notices.handle({ ...event, count })).toBe(true);
      expect(center.items).toHaveLength(0);
    }
    expect(notices.handle(event)).toBe(true);
    expect(center.items).toHaveLength(1);
    const notice = center.items[0]!;
    expect(notice).toMatchObject({
      key: "learned:web:pinned.example",
      tone: "info",
      title:
        "A tool keeps refusing Puddle's certificate for pinned.example: Puddle has stopped decrypting it 3 times.",
      link: { href: "/workspaces/web/settings", label: "Workspace settings" },
      action: { label: "Never decrypt it here" },
    });
    expect(notice.detail).toContain("web");
    expect(notice.detail).toContain("own list of trusted roots");
    expect(notice.action?.confirm).toBeUndefined();
  });

  it("ends the notice when the host is made permanent, and keeps it when that fails", async () => {
    notices.handle(event);
    api.status = 500;
    await expect(center.items[0]!.action!.run()).rejects.toThrow();
    expect(center.items).toHaveLength(1);
    api.status = 204;
    await center.items[0]!.action!.run();
    expect(center.items).toHaveLength(0);
    expect(api.bodies).toEqual([
      { scope: "workspace" },
      { scope: "workspace" },
    ]);
  });

  it("is one notice per host and workspace, and ignores other events", () => {
    notices.handle(event);
    notices.handle({ ...event, count: 4 });
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
