// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  LEGACY_ANCHORS,
  legacyTarget,
  PAGES,
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
    expect(legacyTarget("#s-disk")).toBe("/settings/workspaces#set-disk-warn");
    expect(legacyTarget("#local-destinations")).toBe(
      "/settings/network#set-local-loopback",
    );
  });

  it("opens Appearance for no anchor or an unknown one", () => {
    expect(legacyTarget("")).toBe("/settings/appearance");
    expect(legacyTarget("#s-nothing")).toBe("/settings/appearance");
  });
});
