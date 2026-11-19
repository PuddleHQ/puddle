// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  SETTINGS_SECTIONS,
  TABS,
  activeSection,
  activeTab,
  tabHref,
} from "./tabs.ts";

describe("tabs", () => {
  it("starts with the overview and has unique paths", () => {
    expect(TABS[0]).toEqual({ slug: "", label: "Overview" });
    expect(new Set(TABS.map((t) => t.slug)).size).toBe(TABS.length);
  });

  it("has five tabs: Overview, Terminal, Network, Git and Settings", () => {
    expect(TABS.map((t) => t.label)).toEqual([
      "Overview",
      "Terminal",
      "Network",
      "Git",
      "Settings",
    ]);
  });

  it("lists the settings pages with the table first", () => {
    expect(SETTINGS_SECTIONS.map((s) => s.slug)).toEqual([
      "",
      "environment",
      "shell-init",
      "ssh-agent",
      "ports",
    ]);
  });

  it("builds the address of a tab and of a settings page", () => {
    expect(tabHref("web-shop", "")).toBe("/workspaces/web-shop");
    expect(tabHref("web-shop", "network")).toBe("/workspaces/web-shop/network");
    expect(tabHref("a b", "ports")).toBe("/workspaces/a%20b/settings/ports");
    expect(tabHref("web-shop", "environment")).toBe(
      "/workspaces/web-shop/settings/environment",
    );
    expect(tabHref("web-shop", "terminal")).toBe(
      "/workspaces/web-shop/terminal",
    );
  });

  it("finds the tab a path is on, a settings page being on the Settings tab", () => {
    expect(activeTab("/workspaces/w", "w")).toBe("");
    expect(activeTab("/workspaces/w/", "w")).toBe("");
    expect(activeTab("/workspaces/w/network", "w")).toBe("network");
    expect(activeTab("/workspaces/w/settings/shell-init", "w")).toBe(
      "settings",
    );
    expect(activeTab("/workspaces/w/terminal", "w")).toBe("terminal");
    expect(activeTab("/workspaces/w/nope", "w")).toBe("");
    expect(activeTab("/elsewhere", "w")).toBe("");
  });

  it("finds the page of Settings a path is on", () => {
    expect(activeSection("/workspaces/w/settings", "w")).toBe("");
    expect(activeSection("/workspaces/w/settings/ssh-agent", "w")).toBe(
      "ssh-agent",
    );
    expect(activeSection("/workspaces/w/settings/nope", "w")).toBe("");
    expect(activeSection("/workspaces/w/network", "w")).toBe("");
  });
});
