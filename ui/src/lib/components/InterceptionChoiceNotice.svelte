<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { GlobalInterception } from "#lib/stores/interception.svelte.ts";
  import "#lib/theme/controls.css";

  // Shown once to an install that was set up before the choice existed: it keeps decrypting only
  // the hosts it has a reason to read until the user picks a mode in Settings. Either button
  // answers the notice.
  const store = new GlobalInterception();
  $effect(() => {
    void store.load();
  });
</script>

{#if store.needsNotice}
  <div class="banner" role="status">
    <p>
      New: you can let Puddle decrypt every HTTPS host, not only the ones it has
      a reason to read. Today it decrypts only those, which limits what a rule
      on another host can do.
      <a href="/settings#s-net">Choose in Settings</a>
    </p>
    <button type="button" class="btn" onclick={() => void store.dismissNotice()}
      >Keep it as it is</button
    >
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    padding: var(--space-2) var(--space-8);
    background: var(--color-surface-raised);
    border-bottom: 1px solid var(--color-border-subtle);
  }
  p {
    flex: 1;
    margin: 0;
  }
  @media (max-width: 760px) {
    .banner {
      flex-direction: column;
      align-items: stretch;
      padding: var(--space-2) var(--space-4);
    }
  }
</style>
