<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";

  // Says the browser editor's server could not be installed or updated in the workspace, in the
  // host's words (its title names the reason). `detail` adds what it means and when Puddle tries
  // again.
  let { title, detail = null }: { title: string; detail?: string | null } =
    $props();
</script>

<p class="notice" role="alert">
  <TriangleAlert aria-hidden="true" size={16} />
  <span
    ><b>{title}.</b>{#if detail}<span class="detail">{detail}</span>{/if}</span
  >
</p>

<style>
  .notice {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    margin: 0;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
  .detail {
    margin-inline-start: var(--space-1);
  }
  .notice :global(svg) {
    flex: none;
    margin-top: 2px;
  }
</style>
