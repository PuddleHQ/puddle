<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // A setting with a short list of values. A pick applies at once; a refusal puts the old value back.
  import SettingRow from "./SettingRow.svelte";

  let {
    id,
    label,
    desc = null,
    how = null,
    restart = false,
    value,
    options,
    onchange,
  }: {
    id: string;
    label: string;
    desc?: string | null;
    how?: string | null;
    restart?: boolean;
    /** The value that is in force now (as text). */
    value: string;
    options: readonly { value: string; label: string }[];
    /** Applies the pick; resolves false when it was refused. */
    onchange: (next: string) => Promise<boolean | void> | boolean | void;
  } = $props();

  async function pick(select: HTMLSelectElement) {
    const accepted = await onchange(select.value);
    if (accepted === false) select.value = value;
  }
</script>

<SettingRow {id} {label} {desc} {how} {restart}>
  <select
    {id}
    aria-describedby={desc ? `${id}-desc` : undefined}
    {value}
    onchange={(e) => void pick(e.currentTarget)}
  >
    {#each options as option (option.value)}
      <option value={option.value}>{option.label}</option>
    {/each}
  </select>
</SettingRow>
