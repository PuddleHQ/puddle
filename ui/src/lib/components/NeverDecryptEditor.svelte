<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import {
    formatNames,
    parseNames,
    sameNames,
  } from "#lib/interception/names.ts";
  import type { SaveResult } from "#lib/stores/interception.svelte.ts";
  import "#lib/theme/controls.css";

  // One list of names that are never decrypted: a text box with a name on each line and a Save
  // button. The box follows what is stored; typing is kept until it is saved or the stored list
  // changes.
  let {
    label,
    hint,
    names,
    save,
  }: {
    label: string;
    hint: string;
    /** The stored list. */
    names: readonly string[];
    save: (names: string[]) => Promise<SaveResult>;
  } = $props();

  const id = $props.id();
  // The text is the stored list until typed over, and follows the list when that changes.
  let text = $derived(formatNames(names));
  let failure = $state<string | null>(null);
  let saved = $state(false);
  let working = $state(false);

  const typed = $derived(parseNames(text));
  const unchanged = $derived(sameNames(typed, names));

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    working = true;
    failure = null;
    saved = false;
    const result = await save(typed);
    working = false;
    if (result.ok) saved = true;
    else failure = result.message;
  }
</script>

<form onsubmit={submit}>
  <label for="{id}-names">{label}</label>
  <p class="desc" id="{id}-hint">{hint}</p>
  <textarea
    id="{id}-names"
    rows="4"
    spellcheck="false"
    autocomplete="off"
    aria-describedby="{id}-hint"
    bind:value={text}
    oninput={() => (saved = false)}></textarea>
  <div class="row">
    <button type="submit" disabled={working || unchanged}>Keep names</button>
    <span aria-live="polite">
      {#if saved}<span class="saved">Saved.</span>{/if}
      {#if failure}<span class="error" role="alert">{failure}</span>{/if}
    </span>
  </div>
</form>

<style>
  form {
    display: grid;
    gap: var(--space-2);
  }
  label {
    font-weight: 600;
  }
  .desc {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  textarea {
    width: 100%;
    max-width: 32rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font-family: var(--font-mono, monospace);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }
  button {
    min-height: var(--control-size);
  }
  .saved {
    color: var(--color-success);
  }
  .error {
    color: var(--color-danger);
  }
</style>
