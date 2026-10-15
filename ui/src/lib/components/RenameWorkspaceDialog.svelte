<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { displayNameProblem, type Workspace } from "#lib/workspaces/model.ts";
  import FormDialog from "./FormDialog.svelte";

  // Renames a workspace: only the name puddle shows. `onSubmit` returns the service's refusal as
  // text, or `null` when it went through (the dialog then closes).
  let {
    open = $bindable(false),
    workspace,
    all,
    onSubmit,
  }: {
    open?: boolean;
    workspace: Workspace;
    all: readonly Workspace[];
    onSubmit: (text: string) => Promise<string | null>;
  } = $props();

  const id = $props.id();
  let text = $state("");
  let refusal = $state<string | null>(null);
  let touched = $state(false);
  let busy = $state(false);

  // Checked as you type once you have typed, and against the list as it is now.
  const problem = $derived(
    touched ? displayNameProblem(text, workspace, all) : null,
  );
  const shown = $derived(problem ?? refusal);

  $effect(() => {
    if (open) {
      text = workspace.display_name;
      refusal = null;
      touched = false;
    }
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    touched = true;
    if (displayNameProblem(text, workspace, all)) return;
    busy = true;
    try {
      refusal = await onSubmit(text);
      if (refusal === null) open = false;
    } finally {
      busy = false;
    }
  }
</script>

<FormDialog
  bind:open
  title="Rename workspace"
  description="Only the name puddle shows changes. The workspace keeps running, and its rules, settings and disk stay as they are."
>
  <form onsubmit={submit} novalidate>
    <div class="field">
      <label for="{id}-name">Name</label>
      <input
        id="{id}-name"
        type="text"
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
          Identifier: <span class="mono">{workspace.name}</span>. It names the
          disk and does not change.
        </p>
      {/if}
    </div>
    <div class="actions">
      <button type="button" class="btn" onclick={() => (open = false)}
        >Cancel</button
      >
      <button type="submit" class="btn primary" disabled={busy}>Save</button>
    </div>
  </form>
</FormDialog>
