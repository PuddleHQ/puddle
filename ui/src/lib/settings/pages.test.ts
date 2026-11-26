// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  LEGACY_ANCHORS,
  GROUPS,
  legacyTarget,
  movedTarget,
  PAGES,
  pageGroup,
  pageWhere,
  searchSettings,
  SETTINGS,
  settingHref,
} from "./pages.ts";

describe("the settings search", () => {
  it("finds a setting by its name, by a word that is not in its name, and by the page", () => {
    expect(searchSettings("zoom").map((s) => s.id)).toContain("set-zoom");
    expect(searchSettings("vpn").map((s) => s.id)).toContain(
      "set-local-company_network",
    );
    expect(searchSettings("appearance").map((s) => s.id)).toContain(
      "set-theme",
    );
  });

  it("needs every word typed and ignores case", () => {
    expect(searchSettings("DISK warn").map((s) => s.id)).toEqual(
      expect.arrayContaining(["set-disk-percent", "set-disk-warn"]),
    );
    expect(searchSettings("zoom nonsense")).toEqual([]);
    expect(searchSettings("   ")).toEqual([]);
  });

  it("links a result to its page and the setting on it", () => {
    const hit = SETTINGS.find((s) => s.id === "set-sound");
    expect(hit && settingHref(hit)).toBe("/settings/notifications#set-sound");
  });

  it("lists every setting once, on a page that exists", () => {
    const ids = SETTINGS.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length);
    const pages = new Set(PAGES.map((p) => p.id));
    for (const s of SETTINGS) expect(pages.has(s.page)).toBe(true);
  });
});

describe("old links", () => {
  it("sends every old section anchor to the page that now holds it", () => {
    const known = new Set(SETTINGS.map((s) => `${s.page}#${s.id}`));
    for (const [anchor, target] of Object.entries(LEGACY_ANCHORS)) {
      expect(known.has(target), anchor).toBe(true);
    }
    expect(legacyTarget("#s-disk")).toBe(
      "/settings/this-computer#set-disk-warn",
    );
    expect(legacyTarget("#local-destinations")).toBe(
      "/settings/network#set-local-loopback",
    );
  });

  it("opens Appearance for no anchor or an unknown one", () => {
    expect(legacyTarget("")).toBe("/settings/appearance");
    expect(legacyTarget("#s-nothing")).toBe("/settings/appearance");
  });
});

describe("the three headings", () => {
  it("puts every page under one heading and names it in a search result", () => {
    expect(PAGES.map((p) => p.group)).toEqual([
      "app",
      "app",
      "app",
      "new",
      "new",
      "all",
      "all",
      "all",
      "all",
      "all",
    ]);
    expect(Object.values(GROUPS).map((g) => g.heading)).toEqual([
      "Puddle",
      "Defaults for new workspaces",
      "Rules for all workspaces",
    ]);
    expect(pageGroup("git")).toBe("all");
    expect(pageWhere("git")).toBe("Rules for all workspaces: Git and SSH");
  });

  it("keeps the moved settings findable on their new pages", () => {
    const where = (id: string) => SETTINGS.find((s) => s.id === id)?.page;
    expect(where("set-disk-warn")).toBe("this-computer");
    expect(where("set-zoom")).toBe("this-computer");
    expect(where("set-clip")).toBe("workspaces");
    expect(where("set-mise")).toBe("workspaces");
    expect(where("set-ssh-agent")).toBe("git");
    expect(where("set-push-listed")).toBe("git");
    expect(where("set-server")).toBe("editor-server");
    expect(where("set-capture")).toBe("credentials");
    expect(where("set-env")).toBe("environment");
  });
});

describe("old page links", () => {
  it("sends an old page path with a moved setting's hash to the new page", () => {
    expect(movedTarget("/settings/editor", "#set-zoom")).toBe(
      "/settings/this-computer#set-zoom",
    );
    expect(movedTarget("/settings/workspaces/", "#set-env")).toBe(
      "/settings/environment#set-env",
    );
  });

  it("leaves a setting that is on its page, and a hash that is no setting", () => {
    expect(movedTarget("/settings/editor", "#set-editor-theme")).toBeNull();
    expect(movedTarget("/settings/network", "#local-destinations")).toBeNull();
    expect(movedTarget("/settings/network", "")).toBeNull();
  });
});
