<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { Snippet } from "svelte";

  let { title, children }: { title: string; children?: Snippet } = $props();
</script>

<section class="empty">
  <h1>{title}</h1>
  <p>
    {#if children}{@render children()}{:else}Nothing here yet.{/if}
  </p>
</section>

<style>
  h1 {
    margin-bottom: var(--space-2);
  }
  p {
    margin: 0;
    color: var(--color-text-muted);
  }
</style>
