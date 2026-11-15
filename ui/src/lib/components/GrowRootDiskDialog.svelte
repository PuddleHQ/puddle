<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { untrack } from "svelte";
  import {
    formatMib,
    rootDiskGrowProblem,
    suggestedRootDiskGib,
    type Workspace,
  } from "#lib/workspaces/model.ts";
  import FormDialog from "./FormDialog.svelte";

  // Makes a workspace's root disk bigger. `onSubmit` returns the service's refusal as text, or
  // `null` when it took the size (the dialog then closes).
  let {
    open = $bindable(false),
    workspace,
    onSubmit,
  }: {
    open?: boolean;
    workspace: Workspace;
    onSubmit: (gib: number) => Promise<string | null>;
  } = $props();

  const id = $props.id();
  let text = $state("");
  let refusal = $state<string | null>(null);
  let touched = $state(false);
  let busy = $state(false);

  const problem = $derived(
    touched ? rootDiskGrowProblem(text, workspace) : null,
  );
  const shown = $derived(problem ?? refusal);
  const running = $derived(workspace.status === "running");

  // Start from the suggestion each time the dialog opens, not each time the live record is
  // replaced (a refusal makes the page read the list again, and must not wipe the refusal).
  let wasOpen = false;
  $effect(() => {
    if (open && !wasOpen) {
      text = String(untrack(() => suggestedRootDiskGib(workspace)));
      refusal = null;
      touched = false;
    }
    wasOpen = open;
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    touched = true;
    if (rootDiskGrowProblem(text, workspace)) return;
    busy = true;
    try {
      refusal = await onSubmit(Number(text.trim()));
      if (refusal === null) open = false;
    } finally {
      busy = false;
    }
  }
</script>

<FormDialog
  bind:open
  title="Make the root disk of {workspace.name} bigger"
  description={running
    ? "The root disk holds the home directory, tool caches and installed packages. The workspace has the room at once when it can, otherwise Puddle restarts it; nothing saved to disk is lost."
    : "The root disk holds the home directory, tool caches and installed packages. The workspace has the room when it starts next."}
>
  <form onsubmit={submit} novalidate>
    <div class="field">
      <label for="{id}-size"
        >New size in GiB (now {formatMib(
          workspace.root_disk_size_mib ?? 0,
        )})</label
      >
      <input
        id="{id}-size"
        type="text"
        inputmode="numeric"
        autocomplete="off"
        spellcheck="false"
        bind:value={text}
        oninput={() => {
          touched = true;
          refusal = null;
        }}
        aria-invalid={shown ? "true" : undefined}
        aria-describedby={shown ? `${id}-error` : `${id}-hint`}
      />
      {#if shown}
        <p class="error" id="{id}-error" role="alert">{shown}</p>
      {:else}
        <p class="hint" id="{id}-hint">
          A root disk only grows, and keeps its size when it is reset. Puddle
          refuses a size this computer has no room for.
        </p>
      {/if}
    </div>
    <div class="actions">
      <button type="button" class="btn" onclick={() => (open = false)}
        >Cancel</button
      >
      <button type="submit" class="btn primary" disabled={busy}
        >Make bigger</button
      >
    </div>
  </form>
</FormDialog>
