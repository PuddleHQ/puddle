// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { documentTitle, frameFor, NAV, sectionFor } from "./nav.ts";

describe("nav", () => {
  it("has the top-level sections, in order, the owner's Admin last", () => {
    expect(NAV.map((n) => n.label)).toEqual([
      "Workspaces",
      "Inbox",
      "Rules",
      "Activity",
      "Identities",
      "Settings",
      "Admin",
    ]);
    expect(new Set(NAV.map((n) => n.href)).size).toBe(NAV.length);
  });

  it("finds the section of a path, including nested ones", () => {
    expect(sectionFor("/rules")?.label).toBe("Rules");
    expect(sectionFor("/workspaces/abc/network")?.label).toBe("Workspaces");
    expect(sectionFor("/rulesx")).toBeUndefined();
    expect(sectionFor("/")).toBeUndefined();
  });

  it("titles pages, with the pending count only on the inbox", () => {
    expect(documentTitle("/inbox", 0)).toBe("Inbox - Puddle");
    expect(documentTitle("/inbox", 3)).toBe("Inbox (3) - Puddle");
    expect(documentTitle("/rules", 3)).toBe("Rules - Puddle");
    expect(documentTitle("/nope", 3)).toBe("Not found - Puddle");
  });

  it("titles the first-run steps by their name, with no pending count", () => {
    expect(documentTitle("/welcome", 3)).toBe("Welcome - Puddle");
    expect(documentTitle("/welcome/connect", 3)).toBe("Connect - Puddle");
    expect(documentTitle("/welcome/unknown", 0)).toBe("Welcome - Puddle");
  });
});

describe("the page frame", () => {
  it("gives forms the reading width and tables, the terminal and a workspace's tabs the full width", () => {
    for (const path of [
      "/workspaces",
      "/workspaces/abc",
      "/workspaces/abc/network",
      "/workspaces/abc/terminal",
      "/workspaces/abc/settings/ports",
      "/inbox",
      "/rules",
      "/activity",
      "/settings",
      "/settings/appearance",
      "/settings/network-health",
      "/settings/app-errors",
      "/welcome/check",
    ]) {
      expect(frameFor(path), path).toBe("full");
    }
    for (const path of [
      "/identities",
      "/identities/abc",
      "/admin",
      "/invite",
      "/no-such-page",
    ]) {
      expect(frameFor(path), path).toBe("reading");
    }
  });
});
