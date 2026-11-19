// SPDX-License-Identifier: AGPL-3.0-or-later
// A workspace's own settings as the table shows them: the value in effect is what each dropdown
// shows, and a choice becomes an override. Following the global setting again is a separate
// reset beside the row, so no option has to say "use the global setting" in place of its value.
import type { components } from "#lib/api/schema.d.ts";
import { cpuCounts, cpuLabel } from "#lib/settings/cpus.ts";
import { formatMib } from "./model.ts";

export type Layer = components["schemas"]["SettingsLayer"];
export type Source = components["schemas"]["SettingSource"];
export type Clipboard = components["schemas"]["ClipboardRead"];

/** The memory sizes offered, in MiB. */
export const MEMORY_CHOICES_MIB: readonly number[] = [
  2048, 4096, 8192, 12_288, 16_384, 24_576, 32_768,
];

export interface Option<V extends string> {
  value: V;
  label: string;
}

/** The memory sizes, with the one in effect added when it is not a standard size. */
export function memoryChoices(inEffectMib: number): Option<string>[] {
  const sizes = new Set([...MEMORY_CHOICES_MIB, inEffectMib]);
  return [...sizes]
    .sort((a, b) => a - b)
    .map((mib) => ({ value: String(mib), label: formatMib(mib) }));
}

/** 1 up to the host's maximum, and the count in effect when it is above that. */
export function cpuChoices(max: number, inEffect: number): Option<string>[] {
  return cpuCounts(max, inEffect).map((n) => ({
    value: String(n),
    label: cpuLabel(n),
  }));
}

/** A yes-or-no setting as a dropdown value. */
export type OnOff = "on" | "off";

export const onOff = (value: boolean): OnOff => (value ? "on" : "off");

export const fromOnOff = (choice: string): boolean => choice === "on";

/** A local destination or direct SSH: allowed or not. */
export const ALLOW_OPTIONS: Option<OnOff>[] = [
  { value: "on", label: "Allowed" },
  { value: "off", label: "Not allowed" },
];

/** A switch: on or off. */
export const SWITCH_OPTIONS: Option<OnOff>[] = [
  { value: "on", label: "On" },
  { value: "off", label: "Off" },
];

export const CLIPBOARD_OPTIONS: Option<Clipboard>[] = [
  { value: "ask", label: "Ask me each time" },
  { value: "allow", label: "Always allow" },
  { value: "deny", label: "Never allow" },
];

const SOURCE_WORDS: Record<Source, string> = {
  workspace: "This workspace",
  global: "Global setting",
  default: "Puddle's default",
};

/** Where a value in effect comes from, in words for the "From" column. */
export const sourceLabel = (source: Source): string => SOURCE_WORDS[source];
