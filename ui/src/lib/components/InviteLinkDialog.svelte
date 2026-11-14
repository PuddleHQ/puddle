<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { absoluteTime } from "#lib/format/relative-time.ts";
  import { inviteLink, isLoopback } from "#lib/admin/model.ts";
  import FormDialog from "./FormDialog.svelte";
  import "#lib/theme/controls.css";

  // The invite link, shown once: nothing can read it back afterwards, so closing this dialog is
  // the last time anyone sees it. The secret is after `#`, which no server log sees.
  let {
    open = $bindable(false),
    name,
    secret,
    expiresAt,
    origin,
    onClose,
  }: {
    open?: boolean;
    name: string;
    secret: string;
    expiresAt: number;
    /** The address this page was opened at. */
    origin: string;
    onClose?: (() => void) | undefined;
  } = $props();

  const id = $props.id();
  const link = $derived(inviteLink(origin, secret));
  let copied = $state(false);
  let copyFailed = $state(false);

  $effect(() => {
    void secret;
    copied = false;
    copyFailed = false;
  });

  function done() {
    open = false;
    onClose?.();
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(link);
      copied = true;
      copyFailed = false;
    } catch {
      copied = false;
      copyFailed = true;
    }
  }
</script>

<FormDialog
  bind:open
  {onClose}
  title="Invite link for {name}"
  description="Give this link to {name}. It works once and until {absoluteTime(
    expiresAt,
  )}. Puddle cannot show it again: close this and the only way to a new one is a new invite."
>
  <div class="field">
    <label for="{id}-link">Invite link</label>
    <input id="{id}-link" type="text" readonly value={link} />
    {#if isLoopback(origin)}
      <p class="hint">
        This link uses the address you opened Puddle at, which {name} can't reach.
        Use your public address in its place; the part after # stays the same.
      </p>
    {/if}
  </div>
  <p class="error" role="alert">
    {copyFailed ? "Couldn't copy it. Select the link and copy it by hand." : ""}
  </p>
  <p class="hint" role="status">{copied ? "Copied." : ""}</p>
  <div class="actions">
    <button type="button" class="btn" onclick={copy}>Copy link</button>
    <button type="button" class="btn primary" onclick={done}>Done</button>
  </div>
</FormDialog>
