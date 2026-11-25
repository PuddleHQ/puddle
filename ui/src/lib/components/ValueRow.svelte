<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts" generics="T">
  // A setting typed into a field. Nothing is sent while typing: when the text differs from what is
  // stored a Save button appears beside the field (Enter presses it), and a value that cannot be
  // used says why and holds the button back.
  import { holdReload } from "#lib/stores/unsaved-edits.ts";
  import SettingRow from "./SettingRow.svelte";

  type Parsed = { ok: true; value: T } | { ok: false; message: string };

  let {
    id,
    label,
    desc = null,
    how = null,
    restart = false,
    value,
    parse,
    onsave,
    width = "narrow",
    inputmode = undefined,
    placeholder = undefined,
    mono = false,
    multiline = false,
  }: {
    id: string;
    label: string;
    desc?: string | null;
    how?: string | null;
    restart?: boolean;
    /** The stored value as text. */
    value: string;
    /** Checks what was typed. */
    parse: (text: string) => Parsed;
    /** Applies the parsed value; resolves false when it was refused. */
    onsave: (parsed: T) => Promise<boolean | void> | boolean | void;
    width?: "narrow" | "wide";
    inputmode?: "numeric" | "text" | undefined;
    placeholder?: string | undefined;
    mono?: boolean;
    /** A box of several lines (a list or a JSON text) instead of one line; Enter then adds a line. */
    multiline?: boolean;
  } = $props();

  // The text follows the stored value unless the user is typing something else.
  let text = $state("");
  let seen = $state<string | null>(null);
  $effect.pre(() => {
    if (seen !== value) {
      seen = value;
      text = value;
    }
  });
  let busy = $state(false);

  const changed = $derived(text.trim() !== value.trim());
  const parsed = $derived(parse(text));
  const problem = $derived(changed && !parsed.ok ? parsed.message : null);
  $effect(() => holdReload(() => changed));

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!changed || !parsed.ok || busy) return;
    busy = true;
    await onsave(parsed.value);
    busy = false;
  }
</script>

<SettingRow {id} {label} {desc} {how} {restart}>
  <form class="entry" onsubmit={(e) => void save(e)}>
    {#if multiline}
      <textarea
        {id}
        rows="4"
        class={["area", mono && "mono"]}
        {placeholder}
        spellcheck="false"
        aria-describedby={[desc && `${id}-desc`, problem && `${id}-problem`]
          .filter(Boolean)
          .join(" ") || undefined}
        aria-invalid={problem ? "true" : undefined}
        bind:value={text}></textarea>
    {:else}
      <input
        {id}
        type="text"
        class={[width, mono && "mono"]}
        {inputmode}
        {placeholder}
        aria-describedby={[desc && `${id}-desc`, problem && `${id}-problem`]
          .filter(Boolean)
          .join(" ") || undefined}
        aria-invalid={problem ? "true" : undefined}
        bind:value={text}
      />
    {/if}
    {#if changed}
      <button
        type="submit"
        class="btn primary"
        aria-label="Save {label}"
        disabled={!parsed.ok || busy}>Save</button
      >
    {/if}
  </form>
  {#if problem}<p class="problem" id="{id}-problem" role="alert">
      {problem}
    </p>{/if}
</SettingRow>

<style>
  .entry {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .narrow {
    width: 6rem;
  }
  .wide {
    width: min(20rem, 100%);
  }
  .area {
    width: min(28rem, 100%);
    min-height: 5rem;
    resize: vertical;
  }
  .problem {
    margin: 0;
    max-width: 24rem;
    color: var(--color-danger);
    font-size: var(--text-sm);
    text-align: end;
  }
</style>
