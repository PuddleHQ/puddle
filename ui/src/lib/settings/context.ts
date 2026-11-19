// SPDX-License-Identifier: AGPL-3.0-or-later
// The saver the Settings screen shares with its rows.
import { getContext, setContext } from "svelte";
import type { SettingsSaver } from "./saver.svelte.ts";

const KEY = Symbol("settings-saver");

export function provideSaver(saver: SettingsSaver): void {
  setContext(KEY, saver);
}

/** The saver of the Settings screen; undefined for a component shown outside it. */
export const useSaver = (): SettingsSaver | undefined =>
  getContext<SettingsSaver | undefined>(KEY);

/** The saver, for a page that only exists inside Settings. */
export function requireSaver(): SettingsSaver {
  const saver = useSaver();
  if (!saver) throw new Error("This page must be shown inside Settings.");
  return saver;
}

/** The context a test hands to `render` so a row can report on `saver`. */
export const saverContext = (
  saver: SettingsSaver,
): Map<symbol, SettingsSaver> => new Map([[KEY, saver]]);
