<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { AlertDialog } from "bits-ui";
  import "#lib/theme/controls.css";

  // Deleting a user cannot be undone, so the owner types the name first (the server checks it too).
  let {
    open = $bindable(false),
    name,
    working = false,
    error = null,
    onConfirm,
    onCancel,
  }: {
    open?: boolean;
    name: string;
    working?: boolean;
    error?: string | null;
    onConfirm: (typed: string) => void;
    onCancel: () => void;
  } = $props();

  const id = $props.id();
  let typed = $state("");
  let cancelButton = $state<HTMLElement | null>(null);

  $effect(() => {
    if (open) typed = "";
  });
</script>

<AlertDialog.Root
  bind:open
  onOpenChange={(next) => {
    if (!next) onCancel();
  }}
>
  <AlertDialog.Portal>
    <AlertDialog.Overlay class="confirm-overlay" />
    <AlertDialog.Content
      class="confirm-content"
      onOpenAutoFocus={(event) => {
        // The safe choice first: Enter right after opening cancels.
        event.preventDefault();
        cancelButton?.focus();
      }}
    >
      <AlertDialog.Title class="confirm-title">Delete {name}?</AlertDialog.Title
      >
      <AlertDialog.Description class="confirm-body">
        <strong class="confirm-summary"
          >This deletes {name}'s workspaces and everything on their disks, their
          devices, settings and rules.</strong
        >
        <span class="confirm-detail"
          >It can't be undone. Their running workspaces are stopped first.</span
        >
      </AlertDialog.Description>
      <div class="field">
        <label for="{id}-typed">Type <b>{name}</b> to confirm</label>
        <input
          id="{id}-typed"
          type="text"
          autocomplete="off"
          bind:value={typed}
        />
      </div>
      {#if error}<p class="confirm-error" role="alert">{error}</p>{/if}
      <div class="confirm-actions">
        <AlertDialog.Cancel class="btn" bind:ref={cancelButton}
          >Cancel</AlertDialog.Cancel
        >
        <button
          type="button"
          class="btn deny"
          disabled={typed !== name || working}
          onclick={() => onConfirm(typed)}
        >
          {working ? "Deleting…" : "Delete user"}
        </button>
      </div>
    </AlertDialog.Content>
  </AlertDialog.Portal>
</AlertDialog.Root>

<style>
  .field {
    display: grid;
    gap: var(--space-1);
  }
  input {
    min-height: var(--control-size);
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font: inherit;
  }
  .btn:disabled {
    cursor: default;
    opacity: 0.6;
  }
</style>
