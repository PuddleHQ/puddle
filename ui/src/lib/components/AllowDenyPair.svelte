<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import Check from "@lucide/svelte/icons/check";
  import X from "@lucide/svelte/icons/x";
  import "#lib/theme/controls.css";

  // Allow and Deny, always as this pair: Allow first, Deny second, the same size, side by side.
  // `filled` is for the one decision on screen; lists draw them outlined.
  let {
    onAllow,
    onDeny,
    allowLabel = "Allow",
    denyLabel = "Deny",
    allowAria,
    denyAria,
    denyOnly = false,
    filled = false,
    icons = false,
  }: {
    onAllow: (event: MouseEvent) => void;
    onDeny: (event: MouseEvent) => void;
    allowLabel?: string;
    denyLabel?: string;
    allowAria?: string;
    denyAria?: string;
    /** The destination can't be approved yet; only Deny is offered. */
    denyOnly?: boolean;
    filled?: boolean;
    icons?: boolean;
  } = $props();
</script>

<span class="pair" class:filled>
  {#if !denyOnly}
    <button
      type="button"
      class="btn allow"
      aria-label={allowAria}
      onclick={onAllow}
    >
      {#if icons}<Check aria-hidden="true" size={16} />{/if}{allowLabel}
    </button>
  {/if}
  <button type="button" class="btn deny" aria-label={denyAria} onclick={onDeny}>
    {#if icons}<X aria-hidden="true" size={16} />{/if}{denyLabel}
  </button>
</span>

<style>
  .pair {
    display: inline-flex;
    gap: var(--space-2);
  }
  .pair > .btn {
    min-width: 4.5rem;
  }
  .filled > .btn.allow {
    background: var(--color-success);
    color: var(--color-surface);
  }
  .filled > .btn.deny {
    background: var(--color-danger);
    color: var(--color-surface);
  }
</style>
