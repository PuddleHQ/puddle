// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { FakeSettings } from "#lib/testing/fake-settings.ts";
import {
  EMPTY_DRAFT,
  isDirty,
  restartNames,
  savePatch,
  setLayer,
  setServer,
  setToggle,
  setTools,
  setUi,
} from "./draft.ts";
import { bodyFrom, type GlobalView } from "./model.ts";

describe("the draft", () => {
  it("is clean until a value differs from the stored one", () => {
    expect(isDirty(EMPTY_DRAFT)).toBe(false);
    const d = setUi(EMPTY_DRAFT, "sound", true, false);
    expect(isDirty(d)).toBe(true);
    expect(isDirty(setUi(d, "sound", false, false))).toBe(false);
    expect(EMPTY_DRAFT.ui).toEqual({});
  });

  it("keeps the five parts apart", () => {
    let d = setLayer(EMPTY_DRAFT, "memory", 4096, 8192);
    d = setToggle(d, "private", true, false);
    d = setServer(d, "auto_update", false, true);
    d = setUi(d, "theme", "dark", "system");
    expect(d).toEqual({
      layer: { memory: 4096 },
      toggles: { private: true },
      server: { auto_update: false },
      tools: {},
      ui: { theme: "dark" },
    });
  });

  it("names only the changed settings that need a restart, once each", () => {
    let d = setLayer(EMPTY_DRAFT, "zoom_hotkeys", false, true);
    expect(restartNames(d)).toEqual([]);
    d = setLayer(d, "memory", 4096, 8192);
    d = setLayer(d, "capture_logins", false, true);
    expect(restartNames(d)).toEqual(["Default memory", "Login capture"]);
  });
});

describe("the change a save sends", () => {
  const view = (): GlobalView => {
    const fake = new FakeSettings();
    fake.layer.local_toggles.loopback = true;
    return {
      effective: {} as GlobalView["effective"],
      ui: fake.ui,
      unknown_fields: [],
      vscode_server: fake.vscode,
      tools: fake.tools,
      workspace_defaults: fake.layer,
    };
  };

  it("adds a changed category to the stored ones", () => {
    const d = setToggle(EMPTY_DRAFT, "private", true, false);
    const patch = savePatch(view(), d);
    expect(patch.layer.local_toggles).toMatchObject({
      loopback: true,
      private: true,
      metadata: null,
    });
  });

  it("sends the global mise switch only when it was changed, and putting it back is no change", () => {
    const on = setTools(EMPTY_DRAFT, "mise", false, true);
    expect(isDirty(on)).toBe(true);
    expect(savePatch(view(), on).tools).toEqual({ mise: false });
    expect(isDirty(setTools(on, "mise", true, true))).toBe(false);
    expect(bodyFrom(view(), savePatch(view(), on)).tools).toEqual({
      mise: false,
    });
    expect(bodyFrom(view()).tools).toEqual({ mise: null });
  });

  it("leaves the toggles out when none changed", () => {
    expect(savePatch(view(), EMPTY_DRAFT).layer).toEqual({});
  });
});
