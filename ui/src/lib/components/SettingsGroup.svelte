<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // A titled card of settings on one Settings page.
  import type { Snippet } from "svelte";

  let {
    title,
    id = undefined,
    intro = null,
    children,
  }: {
    title: string;
    /** An anchor for the group as a whole (the search links to it). */
    id?: string | undefined;
    /** One line under the title, when the title alone is not enough. */
    intro?: string | null;
    children: Snippet;
  } = $props();
</script>

<section class="group" {id} aria-label={title}>
  <h2>{title}</h2>
  {#if intro}<p class="intro">{intro}</p>{/if}
  {@render children()}
</section>

<style>
  .group {
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
    scroll-margin-top: var(--space-4);
  }
  h2 {
    margin: 0 0 var(--space-1);
    font-size: var(--text-md);
  }
  .intro {
    margin: 0 0 var(--space-2);
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
</style>
