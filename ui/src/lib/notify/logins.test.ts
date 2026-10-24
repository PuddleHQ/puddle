// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";
import { asLoginProblem, LoginNotices } from "./logins.ts";
import { InAppNotifier } from "./notifier.ts";
import { NoticeCenter } from "./notices.svelte.ts";
import { NoticeWatcher } from "./watcher.ts";

let center: NoticeCenter;
let notices: LoginNotices;

const problem = (kind: string, service = "Claude Code") => ({
  type: "login_problem",
  workspace: "web",
  service,
  kind,
});

beforeEach(() => {
  center = new NoticeCenter();
  notices = new LoginNotices(new InAppNotifier(center));
});

describe("a login Puddle could not capture", () => {
  it.each([
    "store_unavailable",
    "unexpected_answer",
    "unusable_token",
    "bound_token",
  ])("says the login works but is not protected (%s)", (kind) => {
    expect(notices.handle(problem(kind))).toBe(true);
    expect(center.items).toHaveLength(1);
    const notice = center.items[0]!;
    expect(notice).toMatchObject({
      key: `login:web:Claude Code:${kind}`,
      tone: "warning",
      title: "Claude Code login in web is not protected.",
      link: { href: "/workspaces/web/settings", label: "Workspace settings" },
    });
    expect(notice.detail).toContain("real token");
    expect(notice.detail).toMatch(/^[A-Z]/);
  });

  it("names the store as the way out when the store is the problem", () => {
    notices.handle(problem("store_unavailable"));
    expect(center.items[0]!.detail).toContain("credential store");
    expect(center.items[0]!.detail).toContain("sign in again");
  });
});

describe("a problem Puddle has no name for", () => {
  it("says there is a problem without claiming what it is", () => {
    notices.handle(problem("unknown"));
    const notice = center.items[0]!;
    expect(notice.title).toBe("Claude Code login in web has a problem.");
    expect(notice.detail).toContain("cannot describe");
  });
});

describe("a login Puddle kept and cannot use", () => {
  it("says the tool will ask for a new sign-in when a kept login cannot be read", () => {
    notices.handle(problem("unreadable", "GitHub"));
    const notice = center.items[0]!;
    expect(notice.title).toBe("GitHub login in web can't be used.");
    expect(notice.detail).toContain("ask you to sign in again");
  });

  it("says the stand-in is refused and the way out when capture was turned off after a login", () => {
    notices.handle(problem("capture_off", "GitHub"));
    const notice = center.items[0]!;
    expect(notice).toMatchObject({
      key: "login:web:GitHub:capture_off",
      title: "GitHub login in web can't be used.",
      link: { href: "/workspaces/web/settings", label: "Workspace settings" },
    });
    expect(notice.detail).toContain("capture is off");
    expect(notice.detail).toContain("Turn login capture on");
    expect(notice.detail).toContain("sign in again");
  });

  it("raises one notice for the same problem, not a pile", () => {
    notices.handle(problem("unreadable"));
    notices.handle(problem("unreadable"));
    expect(center.items).toHaveLength(1);
  });
});

describe("events that are not login problems", () => {
  it.each([
    null,
    "text",
    { type: "oom_kill" },
    { type: "login_problem", workspace: "web", service: "GitHub" },
    {
      type: "login_problem",
      workspace: 1,
      service: "GitHub",
      kind: "unreadable",
    },
    { type: "login_problem", workspace: "web", service: "GitHub", kind: "new" },
  ])("are ignored (%j)", (raw) => {
    expect(asLoginProblem(raw)).toBeNull();
    expect(notices.handle(raw)).toBe(false);
    expect(center.items).toHaveLength(0);
  });
});

describe("the notice watcher", () => {
  it("turns the stream's event into the notice", () => {
    const watcher = new NoticeWatcher({ notifier: new InAppNotifier(center) });
    watcher.handle(problem("bound_token", "GitHub"));
    expect(center.items).toHaveLength(1);
    expect(center.items[0]!.title).toBe(
      "GitHub login in web is not protected.",
    );
  });
});

describe("forgetting a kept login from its notice", () => {
  type Reply = { error?: { message: string } };
  const forgetting = (reply: () => Promise<Reply>) => {
    const DELETE = vi.fn(reply);
    const login = new LoginNotices(new InAppNotifier(center), {
      DELETE,
    } as never);
    return { DELETE, login };
  };

  it.each(["capture_off", "unreadable"])(
    "offers Forget on a %s notice and asks first, saying what goes",
    (kind) => {
      const { login } = forgetting(() => Promise.resolve({}));
      login.handle(problem(kind));
      const action = center.items[0]!.action!;
      expect(action.label).toBe("Forget Claude Code login");
      expect(action.confirm).toMatchObject({
        title: "Forget the Claude Code login in web?",
        confirmLabel: "Forget login",
      });
      expect(action.confirm!.summary).toContain("credential store");
      expect(action.confirm!.detail).toContain("sign in again");
    },
  );

  it.each([
    "store_unavailable",
    "unexpected_answer",
    "unusable_token",
    "bound_token",
    "unknown",
  ])("offers nothing to forget on a %s notice", (kind) => {
    notices.handle(problem(kind));
    expect(center.items[0]!.action).toBeUndefined();
  });

  it("deletes the login by workspace and the service's name, then ends the notice", async () => {
    const { DELETE, login } = forgetting(() => Promise.resolve({}));
    login.handle(problem("capture_off"));
    await center.items[0]!.action!.run();
    expect(DELETE).toHaveBeenCalledWith(
      "/api/workspaces/{id}/logins/{service}",
      { params: { path: { id: "web", service: "Claude Code" } } },
    );
    expect(center.items).toEqual([]);
  });

  it("rejects with the API's own words as a sentence and keeps the notice when the store refuses", async () => {
    const { login } = forgetting(() =>
      Promise.resolve({
        error: {
          message:
            "the operating system's credential store did not delete a saved login; unlock it or start it and ask again",
        },
      }),
    );
    login.handle(problem("capture_off"));
    await expect(center.items[0]!.action!.run()).rejects.toThrow(
      "The operating system's credential store did not delete a saved login; unlock it or start it and ask again.",
    );
    expect(center.items).toHaveLength(1);
  });

  it("says so when the service does not answer, and when the error has no words", async () => {
    const down = forgetting(() => Promise.reject(new Error("fetch failed")));
    down.login.handle(problem("capture_off"));
    await expect(center.items[0]!.action!.run()).rejects.toThrow(
      "Puddle's service isn't answering. Try again.",
    );
    const bare = forgetting(() => Promise.resolve({ error: { message: "" } }));
    bare.login.handle(problem("unreadable"));
    await expect(center.items.at(-1)!.action!.run()).rejects.toThrow(
      "Puddle couldn't forget the login.",
    );
  });
});
