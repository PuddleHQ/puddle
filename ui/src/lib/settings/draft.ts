// SPDX-License-Identifier: AGPL-3.0-or-later
// The edits on the Settings screen that are not saved yet. The screen shows a stored value with
// the draft laid over it, and Save sends the draft as one change to the settings document.
import type { LocalCategory } from "#lib/decision/local-labels.ts";
import {
  type GlobalTools,
  type Limits,
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
  tools: Partial<GlobalTools>;
  limits: Partial<Limits>;
}

export const EMPTY_DRAFT: Draft = {
  layer: {},
  toggles: {},
  server: {},
  ui: {},
  tools: {},
  limits: {},
};

/** Settings that act only when a workspace next starts, with the name the notes use. */
export const NEEDS_RESTART: Partial<Record<LayerKey, string>> = {
  memory: "Default memory",
  cpus: "Default CPUs",
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

export const setTools = <K extends keyof GlobalTools>(
  draft: Draft,
  key: K,
  value: GlobalTools[K],
  stored: GlobalTools[K],
): Draft => ({ ...draft, tools: put(draft.tools, key, value, stored) });

export const setLimits = <K extends keyof Limits>(
  draft: Draft,
  key: K,
  value: Limits[K],
  stored: Limits[K],
): Draft => ({ ...draft, limits: put(draft.limits, key, value, stored) });

export function isDirty(draft: Draft): boolean {
  return (
    Object.keys(draft.layer).length > 0 ||
    Object.keys(draft.toggles).length > 0 ||
    Object.keys(draft.server).length > 0 ||
    Object.keys(draft.ui).length > 0 ||
    Object.keys(draft.tools).length > 0 ||
    Object.keys(draft.limits).length > 0
  );
}

/** The draft once `sent` is saved: only the edits made after it was taken, or changed again since. */
export function remaining(sent: Draft, now: Draft): Draft {
  const keep = <T extends object>(was: T, is: T): T =>
    Object.fromEntries(
      Object.entries(is).filter(
        ([key, value]) =>
          !(key in was) || (was as Record<string, unknown>)[key] !== value,
      ),
    ) as T;
  return {
    layer: keep(sent.layer, now.layer),
    toggles: keep(sent.toggles, now.toggles),
    server: keep(sent.server, now.server),
    ui: keep(sent.ui, now.ui),
    tools: keep(sent.tools, now.tools),
    limits: keep(sent.limits, now.limits),
  };
}

/**
 * The parts of the settings document a save replaces, built from the draft. The changed
 * categories travel as they are: `bodyFrom` adds them to the stored ones when the save is sent,
 * since saves queue and the stored ones may have changed while this one waited.
 */
export function savePatch(draft: Draft) {
  return {
    layer: draft.layer,
    toggles: draft.toggles,
    server: draft.server,
    ui: draft.ui,
    tools: draft.tools,
    limits: draft.limits,
  };
}

/** The names of the changed settings that apply only at a workspace's next start. */
export function restartNames(draft: Draft): string[] {
  return (Object.keys(draft.layer) as LayerKey[]).flatMap((key) => {
    const name = NEEDS_RESTART[key];
    return name ? [name] : [];
  });
}
