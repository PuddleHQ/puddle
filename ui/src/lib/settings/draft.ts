// SPDX-License-Identifier: GPL-3.0-or-later
// The edits on the Settings screen that are not saved yet. The screen shows a stored value with
// the draft laid over it, and Save sends the draft as one change to the settings document.
import type { LocalCategory } from "#lib/decision/local.ts";
import {
  type GlobalView,
  type Layer,
  type Ui,
  type VsCodeServer,
} from "./model.ts";

export type LayerKey = Exclude<keyof Layer, "local_toggles">;

export interface Draft {
  layer: Partial<Omit<Layer, "local_toggles">>;
  toggles: Partial<Record<LocalCategory, boolean>>;
  server: Partial<VsCodeServer>;
  ui: Partial<Ui>;
}

export const EMPTY_DRAFT: Draft = {
  layer: {},
  toggles: {},
  server: {},
  ui: {},
};

/** Settings that act only when a workspace next starts, with the name the notes use. */
export const NEEDS_RESTART: Partial<Record<LayerKey, string>> = {
  memory: "Default memory",
  capture_logins: "Login capture",
};

export const RESTART_NOTE = "Applies when a workspace next starts.";

function put<T extends object, K extends keyof T>(
  part: T,
  key: K,
  value: T[K],
  shown: T[K],
): T {
  const next = { ...part };
  // Putting a value back to what is stored is not a change.
  if (value === shown) delete next[key];
  else next[key] = value;
  return next;
}

export const setLayer = <K extends LayerKey>(
  draft: Draft,
  key: K,
  value: Layer[K],
  stored: Layer[K],
): Draft => ({ ...draft, layer: put(draft.layer, key, value, stored) });

export const setToggle = (
  draft: Draft,
  category: LocalCategory,
  value: boolean,
  stored: boolean,
): Draft => ({
  ...draft,
  toggles: put(draft.toggles, category, value, stored),
});

export const setServer = <K extends keyof VsCodeServer>(
  draft: Draft,
  key: K,
  value: VsCodeServer[K],
  stored: VsCodeServer[K],
): Draft => ({ ...draft, server: put(draft.server, key, value, stored) });

export const setUi = <K extends keyof Ui>(
  draft: Draft,
  key: K,
  value: Ui[K],
  stored: Ui[K],
): Draft => ({ ...draft, ui: put(draft.ui, key, value, stored) });

export function isDirty(draft: Draft): boolean {
  return (
    Object.keys(draft.layer).length > 0 ||
    Object.keys(draft.toggles).length > 0 ||
    Object.keys(draft.server).length > 0 ||
    Object.keys(draft.ui).length > 0
  );
}

/** The parts of the settings document a save replaces, built from the draft over the stored view. */
export function savePatch(view: GlobalView, draft: Draft) {
  const toggles = Object.keys(draft.toggles).length > 0;
  return {
    layer: {
      ...draft.layer,
      ...(toggles
        ? {
            local_toggles: {
              ...view.workspace_defaults.local_toggles,
              ...draft.toggles,
            },
          }
        : {}),
    },
    server: draft.server,
    ui: draft.ui,
  };
}

/** The names of the changed settings that apply only at a workspace's next start. */
export function restartNames(draft: Draft): string[] {
  return (Object.keys(draft.layer) as LayerKey[]).flatMap((key) => {
    const name = NEEDS_RESTART[key];
    return name ? [name] : [];
  });
}
