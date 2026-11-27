<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // One setting: its name and one line of what it does on the left, its control on the right, and
  // what happened to the last change beneath the control ("Saved." with Undo, or why it was
  // refused). Anything longer than a line goes behind "How this works".
  import type { Snippet } from "svelte";
  import { useSaver } from "#lib/settings/context.ts";
  import { RESTART_NOTE } from "#lib/settings/draft.ts";
  import "#lib/theme/controls.css";

  let {
    id,
    label,
    desc = null,
    how = null,
    restart = false,
    plain = false,
    stacked = false,
    children,
  }: {
    /** The id of the control inside; the label points at it and the search links to it. */
    id: string;
    label: string;
    /** What the setting does, in one line. */
    desc?: string | null;
    /** The longer account, shown when the user asks for it. */
    how?: string | null;
    /** The change reaches a workspace only when it next starts. */
    restart?: boolean;
    /** The control is a link or button, not a field: the name is plain text and the row is the target. */
    plain?: boolean;
    /** The control is wide (a picker): it sits under the text instead of beside it. */
    stacked?: boolean;
    children: Snippet;
  } = $props();

  const saver = useSaver();
  const state = $derived(saver?.rows[id]);
</script>

<div class="row" class:stacked id={plain ? id : undefined}>
  <div class="text">
    {#if plain}<span class="name">{label}</span>{:else}<label for={id}>
        {label}
      </label>{/if}
    {#if desc}<p class="desc" id="{id}-desc">{desc}</p>{/if}
    {#if restart}<p class="desc restart">{RESTART_NOTE}</p>{/if}
    {#if how}
      <details class="how">
        <summary>
          How this works<span class="visually-hidden"> for {label}</span>
        </summary>
        <p>{how}</p>
      </details>
    {/if}
  </div>
  <div class="control">
    {@render children()}
    <div class="status" role="status" aria-live="polite">
      {#if state?.kind === "saving"}
        <span class="muted">Saving&hellip;</span>
      {:else if state?.kind === "saved"}
        <span class="saved">Saved.</span>
        {#if state.note}<span class="muted">{state.note}</span>{/if}
        {#if state.undo}
          <button
            type="button"
            class="undo"
            onclick={() => void saver?.undo(id)}
          >
            Undo<span class="visually-hidden"> {label}</span>
          </button>
        {/if}
      {/if}
    </div>
    {#if state?.kind === "failed"}
      <p class="failed" role="alert">{state.message}</p>
    {/if}
  </div>
</div>

<style>
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: start;
    gap: var(--space-2) var(--space-6);
    padding-block: var(--space-2);
    border-top: 1px solid var(--color-border-subtle);
  }
  .row:first-child {
    border-top: 0;
  }
  .row.stacked {
    grid-template-columns: minmax(0, 1fr);
  }
  .row.stacked .control {
    justify-items: start;
  }
  .text {
    display: grid;
    gap: var(--space-1);
    min-width: 0;
  }
  label,
  .name {
    font-weight: 600;
  }
  .desc,
  .how p {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .how summary {
    color: var(--color-accent);
    font-size: var(--text-sm);
    cursor: pointer;
    width: fit-content;
  }
  .how p {
    margin-top: var(--space-1);
    max-width: 60ch;
  }
  .control {
    display: grid;
    justify-items: end;
    gap: var(--space-1);
    min-width: 0;
  }
  .status {
    min-height: 1.25rem;
    font-size: var(--text-sm);
  }
  .muted {
    color: var(--color-text-muted);
  }
  .saved {
    color: var(--color-success);
  }
  .failed {
    margin: 0;
    max-width: 24rem;
    color: var(--color-danger);
    font-size: var(--text-sm);
    text-align: end;
  }
  .undo {
    padding: 0 var(--space-1);
    border: 0;
    background: none;
    color: var(--color-accent);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  @media (max-width: 760px) {
    .row {
      grid-template-columns: minmax(0, 1fr);
    }
    .control {
      justify-items: start;
    }
  }
</style>
