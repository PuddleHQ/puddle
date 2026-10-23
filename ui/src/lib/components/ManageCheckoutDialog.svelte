<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import type { Identity } from "#lib/identities/model.ts";
  import FormDialog from "./FormDialog.svelte";

  // "Manage this": shows what Puddle will list for a checkout that is already on the disk (the
  // address it saw, the identity that serves it, the Pull and Push boxes) before anything
  // changes. Nothing in the checkout is touched. `onSubmit` returns the service's refusal as text,
  // or `null` when it went through (the dialog then closes).
  let {
    open = $bindable(false),
    directory,
    url,
    identities,
    chosen,
    onSubmit,
    returnFocusTo,
  }: {
    open?: boolean;
    directory: string;
    /** The address the listing showed: it goes back with the request, and a changed remote refuses. */
    url: string;
    identities: readonly Identity[];
    /** The identity Puddle would pick when none is chosen, or `null` when no identity covers it. */
    chosen: Identity | null;
    onSubmit: (choice: {
      identity: number | null;
      pull: boolean;
      push: boolean;
    }) => Promise<string | null>;
    /** Where focus goes on close, when the checkout's own button is gone by then. */
    returnFocusTo: () => HTMLElement | null | undefined;
  } = $props();

  const id = $props.id();
  let identity = $state("auto");
  let pull = $state(true);
  let push = $state(true);
  let refusal = $state<string | null>(null);
  let busy = $state(false);

  let wasOpen = false;
  $effect(() => {
    if (open && !wasOpen) {
      identity = "auto";
      pull = true;
      push = true;
      refusal = null;
    }
    wasOpen = open;
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    try {
      refusal = await onSubmit({
        identity: identity === "auto" ? null : Number.parseInt(identity, 10),
        pull,
        push,
      });
      if (refusal === null) open = false;
    } finally {
      busy = false;
    }
  }
</script>

<FormDialog
  bind:open
  title="Manage {directory}"
  {returnFocusTo}
  description="Puddle adds this checkout to the workspace's list. Nothing in the checkout changes: its files, branches and remotes stay as they are."
>
  <form onsubmit={submit} novalidate>
    <p class="address">
      Address: <span class="mono">{url}</span>
    </p>
    <div class="field">
      <label for="{id}-identity">Git identity</label>
      <select id="{id}-identity" bind:value={identity}>
        <option value="auto"
          >Let Puddle choose ({chosen
            ? chosen.label
            : "no identity covers this"})</option
        >
        {#each identities as option (option.id)}
          <option value={String(option.id)}>{option.label}</option>
        {/each}
      </select>
    </div>
    <fieldset>
      <legend>Git requests to this repository</legend>
      <label class="choice"
        ><input type="checkbox" bind:checked={pull} /> Pull</label
      >
      <label class="choice"
        ><input type="checkbox" bind:checked={push} /> Push</label
      >
      <p class="hint">
        Fetches and pushes are allowed or refused by these boxes when the
        workspace's "only listed" switches are on.
      </p>
    </fieldset>
    {#if refusal}<p class="error" role="alert">{refusal}</p>{/if}
    <div class="actions">
      <button type="button" class="btn" onclick={() => (open = false)}
        >Cancel</button
      >
      <button type="submit" class="btn primary" disabled={busy}
        >Manage this</button
      >
    </div>
  </form>
</FormDialog>

<style>
  .address {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>
