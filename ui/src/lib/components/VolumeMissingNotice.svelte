<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  let { id, detail = false }: { id: string; detail?: boolean } = $props();
</script>

<div class="notice" role="status">
  <p>
    <b>Its disk is gone, so it can't start.</b>
    {#if detail}
      Restore its volume and start it again, or delete the workspace under
      Danger in Settings; nothing is lost by deleting it.
    {:else}
      Restore its volume and start it again, or open Details to delete it.
    {/if}
  </p>
  <details>
    <summary>Volume name</summary>
    <span class="mono">ws-{id}</span>
  </details>
</div>

<style>
  .notice p {
    margin: 0;
  }
  .notice {
    margin: 0;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
    overflow-wrap: anywhere;
  }
</style>
