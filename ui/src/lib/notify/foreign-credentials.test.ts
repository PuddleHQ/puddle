// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import {
  asForeignCredential,
  ForeignCredentialNotices,
  KIND_NAME,
} from "./foreign-credentials.ts";
import { InAppNotifier } from "./notifier.ts";
import { NoticeCenter } from "./notices.svelte.ts";
import { NoticeWatcher } from "./watcher.ts";

let center: NoticeCenter;
let notices: ForeignCredentialNotices;

const event = (kind: string, host = "api.github.com") => ({
  type: "foreign_credential",
  workspace: "web",
  host,
  kind,
});

beforeEach(() => {
  center = new NoticeCenter();
  notices = new ForeignCredentialNotices(new InAppNotifier(center));
});

describe("a credential Puddle did not add", () => {
  it.each(Object.keys(KIND_NAME))(
    "names the website and the kind, says the request went out, and links to the setting (%s)",
    (kind) => {
      expect(notices.handle(event(kind))).toBe(true);
      expect(center.items).toHaveLength(1);
      const notice = center.items[0]!;
      expect(notice).toMatchObject({
        key: `foreign-credential:web:api.github.com:${kind}`,
        tone: "warning",
        link: { href: "/workspaces/web/settings", label: "Workspace settings" },
      });
      expect(notice.title).toContain("api.github.com");
      expect(notice.title).toContain(KIND_NAME[kind as keyof typeof KIND_NAME]);
      expect(notice.detail).toContain("went out as it was");
      expect(notice.detail).toMatch(/^[A-Z]/);
    },
  );

  it("says a stand-in is refused by the website, and for the others that it may be the user's own", () => {
    notices.handle(event("stale_stand_in", "example.com"));
    expect(center.items[0]!.detail).toContain("example.com will refuse it");
    center.items.length = 0;
    notices.handle(event("github_token"));
    expect(center.items[0]!.detail).toContain("whether it is yours");
    expect(center.items[0]!.detail).toContain("turn this warning off");
  });

  it("raises one notice for a workspace, website and kind however often it comes", () => {
    notices.handle(event("github_token"));
    notices.handle(event("github_token"));
    expect(center.items).toHaveLength(1);
    notices.handle(event("npm_token"));
    notices.handle(event("github_token", "gist.github.com"));
    expect(center.items).toHaveLength(3);
  });

  it("ignores events that are not one, or whose fields have the wrong type", () => {
    expect(notices.handle({ type: "login_problem" })).toBe(false);
    expect(notices.handle(event("a_future_kind"))).toBe(false);
    expect(notices.handle({ ...event("github_token"), host: 3 })).toBe(false);
    expect(asForeignCredential(null)).toBeNull();
    expect(center.items).toHaveLength(0);
  });

  it("is raised from the event stream", () => {
    const watcher = new NoticeWatcher({ notifier: new InAppNotifier(center) });
    watcher.handle(event("gitlab_token", "gitlab.com"));
    expect(center.items).toHaveLength(1);
    expect(center.items[0]!.title).toContain("a GitLab token to gitlab.com");
  });
});
