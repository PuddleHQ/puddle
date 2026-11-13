// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { TABS, activeTab, tabHref } from "./tabs.ts";

describe("tabs", () => {
  it("starts with the overview and has unique paths", () => {
    expect(TABS[0]).toEqual({ slug: "", label: "Overview" });
    expect(new Set(TABS.map((t) => t.slug)).size).toBe(TABS.length);
  });

  it("puts Terminal right after the overview, as the second of eight tabs", () => {
    expect(TABS.map((t) => t.slug).slice(0, 2)).toEqual(["", "terminal"]);
    expect(TABS).toHaveLength(8);
  });

  it("builds the address of a tab", () => {
    expect(tabHref("web-shop", "")).toBe("/workspaces/web-shop");
    expect(tabHref("web-shop", "network")).toBe("/workspaces/web-shop/network");
    expect(tabHref("a b", "ports")).toBe("/workspaces/a%20b/ports");
    expect(tabHref("web-shop", "terminal")).toBe(
      "/workspaces/web-shop/terminal",
    );
  });

  it("finds the tab a path is on", () => {
    expect(activeTab("/workspaces/w", "w")).toBe("");
    expect(activeTab("/workspaces/w/", "w")).toBe("");
    expect(activeTab("/workspaces/w/network", "w")).toBe("network");
    expect(activeTab("/workspaces/w/shell-init", "w")).toBe("shell-init");
    expect(activeTab("/workspaces/w/terminal", "w")).toBe("terminal");
    expect(activeTab("/workspaces/w/nope", "w")).toBe("");
    expect(activeTab("/elsewhere", "w")).toBe("");
  });
});
