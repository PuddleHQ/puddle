<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { AlertDialog } from "bits-ui";
  import "#lib/theme/controls.css";

  let cancelButton = $state<HTMLElement | null>(null);

  let {
    open = $bindable(false),
    title,
    summary,
    detail,
    confirmLabel,
    tone = "allow",
    working = false,
    error = null,
    holdOpen = false,
    closeFocus,
    onConfirm,
    onCancel,
  }: {
    open?: boolean;
    title: string;
    /** The one-line statement of what will happen. */
    summary: string;
    detail?: string;
    confirmLabel: string;
    tone?: "allow" | "deny";
    /** The confirmed work is under way: the confirm button waits. */
    working?: boolean;
    /** Why the confirmed work failed; shown in the dialog, which then stays open for another try. */
    error?: string | null;
    /** The caller closes the dialog when the work is done, so a failure can be read in it. */
    holdOpen?: boolean;
    /** Where focus goes on close when the control that opened the dialog is gone. */
    closeFocus?: () => HTMLElement | null | undefined;
    onConfirm: () => void;
    onCancel?: () => void;
  } = $props();
</script>

<AlertDialog.Root
  bind:open
  onOpenChange={(next) => {
    if (!next) onCancel?.();
  }}
>
  <AlertDialog.Portal>
    <AlertDialog.Overlay class="confirm-overlay" />
    <AlertDialog.Content
      class="confirm-content"
      onCloseAutoFocus={(event) => {
        const target = closeFocus?.();
        if (target) {
          event.preventDefault();
          target.focus();
        }
      }}
      onOpenAutoFocus={(event) => {
        // The safe choice first: Enter right after opening cancels instead of widening a rule.
        event.preventDefault();
        cancelButton?.focus();
      }}
    >
      <AlertDialog.Title class="confirm-title">{title}</AlertDialog.Title>
      <AlertDialog.Description class="confirm-body">
        <strong class="confirm-summary">{summary}</strong>
        {#if detail}<span class="confirm-detail">{detail}</span>{/if}
      </AlertDialog.Description>
      {#if error}<p class="confirm-error" role="alert">{error}</p>{/if}
      <div class="confirm-actions">
        <AlertDialog.Cancel class="btn" bind:ref={cancelButton}
          >Cancel</AlertDialog.Cancel
        >
        <AlertDialog.Action
          class="btn {tone}"
          disabled={working}
          onclick={() => {
            // Action does not close the dialog by itself, unlike Cancel.
            if (!holdOpen) open = false;
            onConfirm();
          }}>{confirmLabel}</AlertDialog.Action
        >
      </div>
    </AlertDialog.Content>
  </AlertDialog.Portal>
</AlertDialog.Root>

<style>
  :global(.confirm-body) {
    display: grid;
    gap: var(--space-2);
    margin: 0;
  }
  :global(.confirm-summary) {
    overflow-wrap: anywhere;
  }
  :global(.confirm-error) {
    margin: 0;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
    overflow-wrap: anywhere;
  }
  :global(.confirm-actions) {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
