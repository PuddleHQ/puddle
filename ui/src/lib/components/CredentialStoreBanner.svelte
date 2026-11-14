<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { CredentialStoreState } from "#lib/stores/credential-store.svelte.ts";
  import "#lib/theme/controls.css";

  // Shown on the first-run screens while the operating system's credential store is locked or
  // missing, so the person sees it before saving a token fails. The host words the cause and the
  // fix; Retry asks it to check again without a restart.
  let { store }: { store: CredentialStoreState } = $props();

  const headingId = $props.id();
</script>

{#if store.problem}
  <section class="banner" aria-labelledby={headingId}>
    <h2 id={headingId}>{store.problem.title}</h2>
    <p>{store.problem.detail}</p>
    <div class="actions">
      <button
        type="button"
        class="btn"
        disabled={store.retrying}
        onclick={() => void store.retry()}
      >
        {store.retrying ? "Checking" : "Retry"}
      </button>
      <span role="status">
        {#if store.retryError}
          {store.retryError}
        {:else if store.stillBroken}
          The credential store still refuses writes.
        {/if}
      </span>
    </div>
  </section>
{/if}

<style>
  .banner {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
    background: var(--color-surface-raised);
  }
  h2,
  p {
    margin: 0;
  }
  h2 {
    font-size: var(--text-md);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }
</style>
