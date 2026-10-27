<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import NeverDecryptEditor from "#lib/components/NeverDecryptEditor.svelte";
  import { GlobalNeverDecrypt } from "#lib/stores/interception.svelte.ts";

  // The names no workspace decrypts, even with "Decrypt every host" on. Workspaces that decrypt
  // every host apply a change at once.
  const store = new GlobalNeverDecrypt();
  $effect(() => {
    void store.load();
  });
</script>

{#if store.status === "failed"}
  <div role="alert">
    <p class="muted">Couldn't read the never-decrypt list.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else if store.status === "ready"}
  <NeverDecryptEditor
    label="Never decrypt, in any workspace"
    hint="Only matters in a workspace that decrypts every host. One host name per line, or *. and a domain such as *.example.com. Hosts with a credential are always decrypted."
    names={store.names}
    save={(names) => store.save(names)}
  />
{/if}

<style>
  .muted {
    margin: 0;
    color: var(--color-text-muted);
  }
  .error {
    margin: 0;
    color: var(--color-danger);
  }
</style>
