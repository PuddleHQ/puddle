<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // A saved on/off preference of the interface (the `ui` part of the settings): shows what is
  // stored, else Puddle's default, and saves a flip at once.
  import { requireSaver } from "#lib/settings/context.ts";
  import { saveUi } from "#lib/settings/apply.ts";
  import type { Ui } from "#lib/settings/model.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";
  import SwitchRow from "./SwitchRow.svelte";

  /** The preferences that are plain on/off switches. */
  type BoolPref = {
    [K in keyof Ui]-?: NonNullable<Ui[K]> extends boolean ? K : never;
  }[keyof Ui];

  let {
    id,
    pref,
    label,
    desc = null,
    how = null,
    fallback,
    disabled = false,
  }: {
    id: string;
    pref: BoolPref;
    label: string;
    desc?: string | null;
    how?: string | null;
    /** Puddle's default, shown while nothing is stored. */
    fallback: boolean;
    disabled?: boolean;
  } = $props();

  const saver = requireSaver();
  const shown = $derived(
    (store.view?.ui[pref] as boolean | null | undefined) ?? fallback,
  );
</script>

<SwitchRow
  {id}
  {label}
  {desc}
  {how}
  {disabled}
  checked={shown}
  onchange={(next) => saveUi(saver, id, pref, next, shown)}
/>
