<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import type { Progress } from "#lib/stores/workspaces.svelte.ts";
  import {
    operationLabel,
    stepLabel,
    stepPosition,
    type Operation,
  } from "#lib/workspaces/model.ts";

  let {
    busy,
    progress,
    repositories,
    onDismiss,
  }: {
    /** What the record says is running, if anything. */
    busy: Operation | null;
    /** What the stream last said. */
    progress: Progress | undefined;
    /** The workspace's list, to say which clone of how many is running. */
    repositories?: readonly { directory: string }[];
    onDismiss?: () => void;
  } = $props();

  const operation = $derived(busy ?? progress?.operation ?? null);
  /** "Cloning api, 2 of 3": which checkout a cloning step is on, and where it is in the list. */
  const clone = $derived.by(() => {
    if (progress?.step !== "cloning" || !progress.repository) return null;
    const dir = progress.repository;
    const total = repositories?.length ?? 0;
    const at = repositories?.findIndex((r) => r.directory === dir) ?? -1;
    return total > 1 && at >= 0
      ? `Cloning ${dir}, ${at + 1} of ${total}`
      : `Cloning ${dir}`;
  });
  const position = $derived(
    operation && progress && !progress.failed
      ? stepPosition(operation, progress.step)
      : null,
  );
</script>

{#if progress?.failed}
  <div class="line failed" role="alert">
    <TriangleAlert aria-hidden="true" size={16} />
    <span class="text">
      <b>{operation ? `${operationLabel(operation)} failed` : "That failed"}.</b
      >
      {#if progress.detail}<span class="detail">{progress.detail}</span>{/if}
    </span>
    {#if onDismiss}
      <button type="button" class="btn" onclick={onDismiss}>Dismiss</button>
    {/if}
  </div>
{:else if busy || progress}
  <p class="line" role="status">
    <LoaderCircle class="spin" aria-hidden="true" size={16} />
    <span class="text">
      <b
        >{progress
          ? (clone ?? stepLabel(progress.step))
          : busy
            ? operationLabel(busy)
            : ""}</b
      >
      {#if position}<span class="muted"
          >step {position.index} of {position.of}</span
        >{/if}
      {#if progress?.detail}<span class="detail">{progress.detail}</span>{/if}
    </span>
  </p>
{/if}

<style>
  .line {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    margin: 0;
    font-size: var(--text-sm);
  }
  .text {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--color-text-muted);
  }
  .detail {
    display: block;
    font-family: var(--font-mono);
    color: var(--color-text-muted);
  }
  .failed {
    color: var(--color-danger);
  }
  .failed .detail {
    color: inherit;
  }
  :global(.spin) {
    flex: none;
    margin-top: 0.15rem;
    animation: spin 1.2s linear infinite;
  }
  /* The spinner is a loop, so shortening its duration would make it flicker: it stops instead. The
     text beside it says what is running. */
  @media (prefers-reduced-motion: reduce) {
    :global(.spin) {
      animation: none;
    }
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
