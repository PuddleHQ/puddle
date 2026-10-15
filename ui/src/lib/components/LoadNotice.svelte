<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // What a screen says when a read failed. Before the first successful read it replaces the
  // content; after one (`stale`) it sits above what was read earlier, which may be out of date.
  import type { Snippet } from "svelte";

  let {
    what,
    message,
    stale = false,
    padded = false,
    children,
  }: {
    /** What was being read, as a plain noun phrase: "the rules". */
    what: string;
    /** The reason, a full sentence (see `readFailure`); `null` when none is known. */
    message: string | null;
    stale?: boolean;
    /** Inside a panel that pads its text. */
    padded?: boolean;
    children?: Snippet;
  } = $props();
</script>

<p class="muted" class:padded role="status">
  {#if stale}
    Couldn't refresh {what}.{message ? ` ${message}` : ""} What is shown may be out
    of date; Puddle keeps trying.
  {:else}
    Couldn't read {what}.{message ? ` ${message}` : ""} Trying again.
  {/if}
  {#if children}{@render children()}{/if}
</p>

<style>
  .padded {
    padding: var(--space-4);
  }
</style>
