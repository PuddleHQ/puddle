// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import {
  asUnknownToken,
  KIND_NAME,
  UnknownTokenNotices,
} from "./unknown-tokens.ts";
import { InAppNotifier } from "./notifier.ts";
import { NoticeCenter } from "./notices.svelte.ts";
import { NoticeWatcher } from "./watcher.ts";

let center: NoticeCenter;
let notices: UnknownTokenNotices;

const event = (kind: string, host = "login.example.com") => ({
  type: "unknown_token_in_answer",
  workspace: "web",
  host,
  kind,
});

beforeEach(() => {
  center = new NoticeCenter();
  notices = new UnknownTokenNotices(new InAppNotifier(center));
});

describe("a token in an answer that Puddle does not hold", () => {
  it.each(Object.keys(KIND_NAME))(
    "names the website and the kind, says the answer went through, and links to the setting (%s)",
    (kind) => {
      expect(notices.handle(event(kind))).toBe(true);
      expect(center.items).toHaveLength(1);
      const notice = center.items[0]!;
      expect(notice).toMatchObject({
        key: `unknown-token:web:login.example.com:${kind}`,
        tone: "warning",
        link: { href: "/workspaces/web/settings", label: "Workspace settings" },
      });
      expect(notice.title).toContain("login.example.com");
      expect(notice.title).toContain(KIND_NAME[kind as keyof typeof KIND_NAME]);
      expect(notice.detail).toContain("went through as it was");
      expect(notice.detail).toContain("turn this warning off");
      expect(notice.detail).toMatch(/^[A-Z]/);
    },
  );

  it("raises one notice for a workspace, website and kind however often it comes", () => {
    notices.handle(event("github_token"));
    notices.handle(event("github_token"));
    expect(center.items).toHaveLength(1);
    notices.handle(event("npm_token"));
    notices.handle(event("github_token", "other.example.com"));
    expect(center.items).toHaveLength(3);
  });

  it("ignores events that are not one, or whose fields have the wrong type", () => {
    expect(notices.handle({ type: "foreign_credential" })).toBe(false);
    expect(notices.handle(event("a_future_kind"))).toBe(false);
    expect(notices.handle({ ...event("github_token"), host: 3 })).toBe(false);
    expect(asUnknownToken(null)).toBeNull();
    expect(center.items).toHaveLength(0);
  });

  it("is raised from the event stream", () => {
    const watcher = new NoticeWatcher({ notifier: new InAppNotifier(center) });
    watcher.handle(event("gitlab_token", "gitlab.example.com"));
    expect(center.items).toHaveLength(1);
    expect(center.items[0]!.title).toContain("a GitLab token");
  });
});
