// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import { SettingsSaver } from "./saver.svelte.ts";

type Outcome = { ok: true } | { ok: false; message: string };

function make(results: Outcome[] = []) {
  const patches: {
    ui?: Record<string, unknown>;
    layer?: Record<string, unknown>;
  }[] = [];
  const store = {
    view: { workspace_defaults: { local_toggles: {} } } as never,
    save: vi.fn(async (patch: (typeof patches)[number]) => {
      patches.push(patch);
      return results.shift() ?? { ok: true as const };
    }),
  };
  return { saver: new SettingsSaver(store as never), store, patches };
}

describe("the settings saver", () => {
  it("sends one edit, not the whole screen, and records that it was saved", async () => {
    const { saver, patches } = make();
    expect(await saver.apply("set-sound", { ui: { sound: true } })).toBe(true);
    expect(patches).toHaveLength(1);
    expect(patches[0]?.ui).toEqual({ sound: true });
    expect(saver.rows["set-sound"]).toMatchObject({ kind: "saved" });
  });

  it("undoes a saved change by sending the value that was there before", async () => {
    const { saver, patches } = make();
    await saver.apply(
      "set-sound",
      { ui: { sound: true } },
      { ui: { sound: false } },
    );
    expect(await saver.undo("set-sound")).toBe(true);
    expect(patches.at(-1)?.ui).toEqual({ sound: false });
    expect(saver.rows["set-sound"]).toMatchObject({
      kind: "saved",
      note: "Undone.",
    });
  });

  it("keeps the refusal on the row and offers no Undo for a change that was not saved", async () => {
    const { saver } = make([{ ok: false, message: "No." }]);
    expect(await saver.apply("set-sound", { ui: { sound: true } })).toBe(false);
    expect(saver.rows["set-sound"]).toEqual({ kind: "failed", message: "No." });
    expect(await saver.undo("set-sound")).toBe(false);
  });

  it("says when a change applies only at the next start of a workspace", async () => {
    const { saver } = make();
    await saver.apply("set-memory", { layer: { memory: 4096 } });
    expect(saver.rows["set-memory"]).toMatchObject({
      note: "Applies when a workspace next starts.",
    });
    await saver.apply("set-sound", { ui: { sound: true } });
    expect(saver.rows["set-sound"]).toMatchObject({ note: null });
  });

  it("refuses while the settings are not loaded", async () => {
    const saver = new SettingsSaver({ view: null, save: vi.fn() } as never);
    expect(await saver.apply("set-sound", { ui: { sound: true } })).toBe(false);
    expect(saver.rows["set-sound"]).toMatchObject({ kind: "failed" });
  });
});
