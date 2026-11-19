<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // A setting that is on or off. It applies the moment it is flipped; a refusal puts it back.
  import SettingRow from "./SettingRow.svelte";

  let {
    id,
    label,
    desc = null,
    how = null,
    restart = false,
    checked,
    disabled = false,
    onchange,
  }: {
    id: string;
    label: string;
    desc?: string | null;
    how?: string | null;
    restart?: boolean;
    /** The value that is in force now. */
    checked: boolean;
    disabled?: boolean;
    /** Applies the new value; resolves false when it was refused. */
    onchange: (next: boolean) => Promise<boolean | void> | boolean | void;
  } = $props();

  async function flip(box: HTMLInputElement) {
    const accepted = await onchange(box.checked);
    // A refusal leaves the stored value as it was, whatever the click did to the box.
    if (accepted === false) box.checked = checked;
  }
</script>

<SettingRow {id} {label} {desc} {how} {restart}>
  <input
    {id}
    type="checkbox"
    role="switch"
    aria-describedby={desc ? `${id}-desc` : undefined}
    {checked}
    {disabled}
    onchange={(e) => void flip(e.currentTarget)}
  />
</SettingRow>
