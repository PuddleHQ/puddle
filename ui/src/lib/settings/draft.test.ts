// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  FAKE_EFFECTIVE_LIMITS,
  FakeSettings,
} from "#lib/testing/fake-settings.ts";
import {
  EMPTY_DRAFT,
  isDirty,
  remaining,
  restartNames,
  savePatch,
  setLayer,
  setLimits,
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

  it("keeps the six parts apart", () => {
    let d = setLayer(EMPTY_DRAFT, "memory", 4096, 8192);
    d = setToggle(d, "private", true, false);
    d = setServer(d, "auto_update", false, true);
    d = setUi(d, "theme", "dark", "system");
    d = setLimits(d, "max_cpus", 6, null);
    expect(d).toEqual({
      layer: { memory: 4096 },
      toggles: { private: true },
      server: { auto_update: false },
      tools: {},
      ui: { theme: "dark" },
      limits: { max_cpus: 6 },
    });
    expect(isDirty(setLimits(EMPTY_DRAFT, "max_cpus", null, null))).toBe(false);
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
      revision: fake.revision(),
      vscode_server: fake.vscode,
      tools: fake.tools,
      mise_available: true,
      limits: fake.limits,
      effective_limits: FAKE_EFFECTIVE_LIMITS,
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

describe("what is left of a draft after a save", () => {
  it("keeps an edit made while the save ran and drops what was sent", () => {
    const sent = setUi(
      setLayer(EMPTY_DRAFT, "memory", 4096, 8192),
      "sound",
      true,
      false,
    );
    const now = setLimits(setLayer(sent, "cpus", 2, 4), "max_cpus", 3, null);
    const left = remaining(sent, now);
    expect(left.layer).toEqual({ cpus: 2 });
    expect(left.ui).toEqual({});
    expect(left.limits).toEqual({ max_cpus: 3 });
    expect(isDirty(remaining(sent, sent))).toBe(false);
  });

  it("keeps a key that was changed again to another value", () => {
    const sent = setLayer(EMPTY_DRAFT, "memory", 4096, 8192);
    const now = setLayer(sent, "memory", 2048, 8192);
    expect(remaining(sent, now).layer).toEqual({ memory: 2048 });
  });
});
