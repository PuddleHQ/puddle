<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { fillVars, type Fill } from "#lib/rules/fill.ts";

  // A rule set's colour next to its name. `chip` paints the whole pill with the colour and puts
  // the ink with the best contrast on it; `dot` puts a small disc before the name and leaves the
  // name in the normal text colour. Either way the name is always written out, so the colour is
  // never the only thing that says which set it is, and the edge is the theme's border colour,
  // so a colour close to the background still shows.
  let {
    name,
    fill,
    variant = "dot",
  }: {
    name: string;
    fill: Fill | undefined;
    variant?: "chip" | "dot";
  } = $props();
</script>

{#if fill === undefined}
  <span class="plain">{name}</span>
{:else if variant === "chip"}
  <span class="set-chip" style={fillVars(fill)}>{name}</span>
{:else}
  <span class="set-dotted" style={fillVars(fill)}
    ><span class="dot" aria-hidden="true"></span>{name}</span
  >
{/if}

<style>
  .set-chip {
    display: inline-block;
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    background: var(--set-fill);
    color: var(--set-ink);
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .set-dotted {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }
  .dot {
    flex: none;
    width: 0.75em;
    height: 0.75em;
    border: 1px solid var(--color-border);
    border-radius: 50%;
    background: var(--set-fill);
  }
</style>
