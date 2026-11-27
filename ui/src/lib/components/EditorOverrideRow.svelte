<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts" generics="T">
  // One editor default in a workspace's settings table: the value in effect in a box, where it
  // comes from, and a way back to the global value. Nothing is sent while typing: a Save button
  // shows beside the box once the text differs, and a text the API would refuse says why first.
  import type { Parsed } from "#lib/settings/editor.ts";
  import { holdReload } from "#lib/stores/unsaved-edits.ts";
  import { sourceLabel, type Source } from "#lib/workspaces/settings.ts";
  import "#lib/theme/controls.css";

  let {
    id,
    label,
    hint,
    value,
    source,
    parse,
    onsave,
    onreset,
    multiline = false,
    placeholder = undefined,
  }: {
    id: string;
    label: string;
    hint: string;
    /** The value in effect, as text. */
    value: string;
    source: Source;
    parse: (text: string) => Parsed<T>;
    /** Saves the parsed text as this workspace's own value; resolves false when refused. */
    onsave: (parsed: T) => Promise<boolean>;
    /** Follows the global value again. */
    onreset: () => Promise<boolean>;
    multiline?: boolean;
    placeholder?: string | undefined;
  } = $props();

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

  async function reset() {
    busy = true;
    await onreset();
    busy = false;
  }
</script>

<tr data-setting={id}>
  <th scope="row">
    <label for="{id}-field">{label}</label>
    <span class="hint" id="{id}-hint">{hint}</span>
  </th>
  <td>
    <form class="entry" onsubmit={(e) => void save(e)}>
      {#if multiline}
        <textarea
          id="{id}-field"
          rows="3"
          class="area mono"
          {placeholder}
          spellcheck="false"
          aria-describedby={[`${id}-hint`, problem && `${id}-problem`]
            .filter(Boolean)
            .join(" ")}
          aria-invalid={problem ? "true" : undefined}
          bind:value={text}></textarea>
      {:else}
        <input
          id="{id}-field"
          type="text"
          class="line"
          {placeholder}
          aria-describedby={[`${id}-hint`, problem && `${id}-problem`]
            .filter(Boolean)
            .join(" ")}
          aria-invalid={problem ? "true" : undefined}
          bind:value={text}
        />
      {/if}
      {#if changed}
        <button
          type="submit"
          class="btn primary"
          aria-label="Save {label}"
          disabled={!parsed.ok || busy}
        >
          Save
        </button>
      {/if}
    </form>
    {#if problem}<p class="problem" id="{id}-problem" role="alert">
        {problem}
      </p>{/if}
  </td>
  <td class="from">
    {#if source !== "default"}<span class="chip">
        {sourceLabel(source)}
      </span>{/if}
    {#if source === "workspace"}
      <button
        type="button"
        class="reset"
        disabled={busy}
        aria-label="Use the global setting again for {label}"
        onclick={() => void reset()}
      >
        Use the global setting
      </button>
    {/if}
  </td>
</tr>

<style>
  tr > th,
  tr > td {
    padding: var(--space-3);
    border-top: 1px solid var(--color-border-subtle);
    text-align: start;
    vertical-align: top;
  }
  th {
    width: 45%;
    font-weight: 600;
  }
  .hint {
    display: block;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
    font-weight: 400;
  }
  .entry {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
  }
  .line {
    width: min(16rem, 100%);
  }
  .area {
    width: min(24rem, 100%);
    resize: vertical;
  }
  .problem {
    margin: var(--space-1) 0 0;
    color: var(--color-danger);
    font-size: var(--text-sm);
  }
  .from {
    white-space: nowrap;
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
  }
  .reset {
    margin-inline-start: var(--space-2);
    padding: 0;
    border: 0;
    background: none;
    color: var(--color-accent);
    font: inherit;
    font-size: var(--text-sm);
    text-decoration: underline;
    cursor: pointer;
  }
  @media (max-width: 760px) {
    th {
      width: auto;
    }
    .from {
      white-space: normal;
    }
  }
</style>
