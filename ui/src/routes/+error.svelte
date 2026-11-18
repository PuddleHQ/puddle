<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { page } from "$app/state";
  import EmptyState from "#lib/components/EmptyState.svelte";

  let copy = $state<"" | "done" | "failed">("");

  async function copyDetail(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copy = "done";
    } catch {
      copy = "failed";
    }
  }
</script>

<EmptyState
  title={page.status === 404 ? "Page not found" : "Something went wrong"}
>
  {#if page.status === 404}
    There is nothing at this address. Use the menu to go to a section.
  {:else}
    {page.error?.message ?? "Unknown error."}
  {/if}
</EmptyState>
{#if page.status !== 404 && page.error?.detail}
  {@const detail = page.error.detail}
  <details>
    <summary>Details</summary>
    <pre>{detail}</pre>
    <button type="button" class="btn" onclick={() => copyDetail(detail)}
      >Copy details</button
    >
    <span role="status"
      >{copy === "done"
        ? "Copied."
        : copy === "failed"
          ? "Couldn't copy them. Select the text and copy it by hand."
          : ""}</span
    >
  </details>
{/if}

<style>
  details {
    max-width: 60rem;
    margin-top: var(--space-4);
  }
  pre {
    overflow-x: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
  }
</style>
