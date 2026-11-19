// SPDX-License-Identifier: AGPL-3.0-or-later
// Small typed wrappers over `SettingsSaver.apply`: one edit to one key of one part of the settings
// document, with the value that puts it back.
import type { LocalCategory } from "#lib/decision/local-labels.ts";
import type { Draft } from "./draft.ts";
import type { SettingsSaver } from "./saver.svelte.ts";

type Part = "layer" | "server" | "ui" | "tools" | "limits";

function part<P extends Part>(name: P) {
  return <K extends keyof Draft[P]>(
    saver: SettingsSaver,
    id: string,
    key: K,
    next: Draft[P][K],
    previous: Draft[P][K],
  ): Promise<boolean> =>
    saver.apply(
      id,
      { [name]: { [key]: next } } as unknown as Partial<Draft>,
      { [name]: { [key]: previous } } as unknown as Partial<Draft>,
    );
}

export const saveUi = part("ui");
export const saveLayer = part("layer");
export const saveServer = part("server");
export const saveTools = part("tools");
export const saveLimits = part("limits");

/** A local destination switch (these live in the layer's `local_toggles`). */
export const saveToggle = (
  saver: SettingsSaver,
  id: string,
  category: LocalCategory,
  next: boolean,
  previous: boolean,
): Promise<boolean> =>
  saver.apply(
    id,
    { toggles: { [category]: next } },
    { toggles: { [category]: previous } },
  );
