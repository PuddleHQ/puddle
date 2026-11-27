<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import FormDialog from "./FormDialog.svelte";
  import "#lib/theme/controls.css";

  // The owner types a name only. The person chooses their own password with the invite link that
  // follows, so the owner never types, sees or sets one.
  let {
    open = $bindable(false),
    working = false,
    error = null,
    onCreate,
  }: {
    open?: boolean;
    working?: boolean;
    error?: string | null;
    onCreate: (name: string) => void;
  } = $props();

  const id = $props.id();
  let name = $state("");
  const trimmed = $derived(name.trim());

  $effect(() => {
    if (open) name = "";
  });

  function submit(event: SubmitEvent) {
    event.preventDefault();
    if (trimmed !== "" && !working) onCreate(trimmed);
  }
</script>

<FormDialog
  bind:open
  title="Add a user"
  description="You give the person a link; they open it and choose their own password. You never see it."
>
  <form onsubmit={submit}>
    <div class="field">
      <label for="{id}-name">Name</label>
      <input
        id="{id}-name"
        type="text"
        autocomplete="off"
        maxlength="64"
        bind:value={name}
        aria-describedby="{id}-hint"
        required
      />
      <p class="hint" id="{id}-hint">
        1 to 64 characters. It is what they sign in with.
      </p>
    </div>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="actions">
      <button type="button" class="btn" onclick={() => (open = false)}>
        Cancel
      </button>
      <button
        type="submit"
        class="btn primary"
        disabled={trimmed === "" || working}
      >
        {working ? "Adding…" : "Add user"}
      </button>
    </div>
  </form>
</FormDialog>
