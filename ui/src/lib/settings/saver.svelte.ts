// SPDX-License-Identifier: AGPL-3.0-or-later
// The one save behaviour of the Settings screen. A switch or a dropdown applies at once; a typed
// value applies when its Save button is pressed. Either way the row says "Saved" beside itself, and
// offers Undo while that is true. There is no page-wide Save bar and no draft: every change is one
// small edit sent to the settings document on its own.
import {
  globalSettings,
  type GlobalSettings,
} from "#lib/stores/global-settings.svelte.ts";
import {
  EMPTY_DRAFT,
  RESTART_NOTE,
  restartNames,
  savePatch,
  type Draft,
} from "./draft.ts";

/** What a change reports: the settings' own answer. */
type Outcome = { ok: true } | { ok: false; message: string };

/** What a row says about its last change. */
export type RowState =
  | { kind: "saving" }
  | {
      kind: "saved";
      note: string | null;
      /** Puts the change back, when it can be put back. */
      undo: (() => Promise<Outcome>) | null;
    }
  | { kind: "failed"; message: string };

/** One edit as the draft helpers hold it: only the parts that change. */
export const edits = (part: Partial<Draft>): Draft => ({
  ...EMPTY_DRAFT,
  ...part,
});

export class SettingsSaver {
  /** The state of each row, by the id of its control. */
  rows = $state<Record<string, RowState>>({});
  readonly #store: Pick<GlobalSettings, "view" | "save">;

  constructor(store: Pick<GlobalSettings, "view" | "save"> = globalSettings) {
    this.#store = store;
  }

  /**
   * Runs one change and shows its outcome on the row: "Saving", then "Saved." with Undo when there
   * is a way back, or why it was refused. Resolves whether it was accepted.
   */
  async track(
    id: string,
    run: () => Promise<Outcome>,
    options: {
      undo?: (() => Promise<Outcome>) | null;
      note?: string | null;
    } = {},
  ): Promise<boolean> {
    this.rows[id] = { kind: "saving" };
    const result = await run();
    if (!result.ok) {
      this.rows[id] = { kind: "failed", message: result.message };
      return false;
    }
    const back = options.undo ?? null;
    this.rows[id] = {
      kind: "saved",
      note: options.note ?? null,
      undo: back
        ? async () => {
            const undone = await back();
            this.rows[id] = undone.ok
              ? { kind: "saved", note: "Undone.", undo: null }
              : { kind: "failed", message: undone.message };
            return undone;
          }
        : null,
    };
    return true;
  }

  /**
   * Saves one edit to the settings document now. `undo` is the edit that puts it back (offered
   * beside the row afterwards).
   */
  apply(
    id: string,
    change: Partial<Draft>,
    undo: Partial<Draft> | null = null,
  ): Promise<boolean> {
    const send = (part: Partial<Draft>) => {
      const view = this.#store.view;
      if (!view)
        return Promise.resolve<Outcome>({
          ok: false,
          message: "Settings aren't loaded.",
        });
      return this.#store.save(savePatch(edits(part)));
    };
    const sent = edits(change);
    return this.track(id, () => send(change), {
      undo: undo ? () => send(undo) : null,
      note: restartNames(sent).length > 0 ? RESTART_NOTE : null,
    });
  }

  /** Puts a saved change back. */
  async undo(id: string): Promise<boolean> {
    const row = this.rows[id];
    if (row?.kind !== "saved" || !row.undo) return false;
    return (await row.undo()).ok;
  }

  /** Forgets what a row said. */
  clear(id: string): void {
    delete this.rows[id];
  }
}

/** `saver.track`, or just the change when a component is used outside Settings (no rows to report on). */
export async function tracked(
  saver: SettingsSaver | undefined,
  id: string,
  run: () => Promise<Outcome>,
  options: {
    undo?: (() => Promise<Outcome>) | null;
    note?: string | null;
  } = {},
): Promise<boolean> {
  if (saver) return saver.track(id, run, options);
  return (await run()).ok;
}
