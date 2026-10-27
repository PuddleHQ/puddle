<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { page } from "$app/state";
  import EnvironmentTable from "#lib/components/EnvironmentTable.svelte";
  import {
    PLAIN_AT_NEXT_START,
    SECRET_AT_ONCE,
    SECRET_NEVER_IN,
  } from "#lib/environment/model.ts";
  import { EnvironmentStore } from "#lib/stores/environment.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import { isDown } from "#lib/workspaces/model.ts";
  import "#lib/theme/controls.css";

  const id = $derived(page.params["id"] ?? "");
  const workspace = $derived(workspaces.list.find((w) => w.id === id));
  const name = $derived(workspace?.name);
  const store = new EnvironmentStore();
  const fid = $props.id();

  $effect(() => {
    if (name) return store.start({ kind: "workspace", name });
  });
</script>

{#if workspace}
  <section aria-labelledby="{fid}-h">
    <h2 id="{fid}-h">Environment</h2>
    <p class="muted">
      Variables the programs in this workspace start with, and secrets Puddle
      keeps for it. This workspace's own come first; the ones every workspace
      gets are listed after them and can be overridden here (or changed in
      <a href="/settings#s-env">Settings</a>).
    </p>
    <p class="muted">
      {#if isDown(workspace.status)}
        Both kinds are read when the workspace starts.
      {:else}
        {PLAIN_AT_NEXT_START} {SECRET_AT_ONCE}
      {/if}
      {SECRET_NEVER_IN}
    </p>
    <EnvironmentTable
      {store}
      label="Environment of {workspace.name}"
      workspace={workspace.name}
      running={!isDown(workspace.status)}
    />
  </section>
{:else}
  <p class="muted">Loading environment&hellip;</p>
{/if}

<style>
  section {
    display: grid;
    gap: var(--space-3);
    margin-bottom: var(--space-6);
    max-width: 60rem;
  }
  h2 {
    font-size: var(--text-lg);
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--color-text-muted);
  }
</style>
