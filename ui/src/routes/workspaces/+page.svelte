<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import LoadNotice from "#lib/components/LoadNotice.svelte";
  import { onMount } from "svelte";
  import Plus from "@lucide/svelte/icons/plus";
  import OrphanVolumesNotice from "#lib/components/OrphanVolumesNotice.svelte";
  import FirstWorkspaceForm from "#lib/components/FirstWorkspaceForm.svelte";
  import WorkspaceRow from "#lib/components/WorkspaceRow.svelte";
  import { orphanVolumes } from "#lib/stores/orphan-volumes.svelte.ts";
  import { pending } from "#lib/stores/pending.svelte.ts";
  import { workspaceActions as actions } from "#lib/stores/workspace-actions.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import "#lib/theme/controls.css";

  let now = $state(Date.now());

  onMount(() => {
    const clock = setInterval(() => {
      now = Date.now();
    }, 30_000);
    const stopOrphans = orphanVolumes.start();
    return () => {
      clearInterval(clock);
      stopOrphans();
    };
  });

  const waitingBy = $derived.by(() => {
    const counts: Record<string, number> = {};
    for (const row of pending.rows)
      counts[row.request.workspace] = (counts[row.request.workspace] ?? 0) + 1;
    return counts;
  });
  const running = $derived(
    workspaces.list.filter((w) => w.status === "running").length,
  );
</script>

<div class="head">
  <div>
    <h1>Workspaces</h1>
    {#if workspaces.status === "ready" && workspaces.list.length > 0}
      <p class="sub">
        {workspaces.list.length}
        {workspaces.list.length === 1 ? "workspace" : "workspaces"}, {running} running
      </p>
    {/if}
  </div>
  {#if workspaces.list.length > 0}
    <button
      type="button"
      class="btn primary"
      onclick={() => actions.openCreate()}
    >
      <Plus aria-hidden="true" size={16} />New workspace
    </button>
  {/if}
</div>

<OrphanVolumesNotice store={orphanVolumes} />

{#if workspaces.status === "ready" && workspaces.loadError}
  <LoadNotice what="the workspaces" message={workspaces.loadError} stale />
{/if}

{#if workspaces.status === "loading"}
  <p class="muted">Loading workspaces&hellip;</p>
{:else if workspaces.status === "failed"}
  <LoadNotice what="the workspaces" message={workspaces.loadError} />
{:else if workspaces.list.length === 0}
  <FirstWorkspaceForm onMore={() => actions.openCreate()} />
{:else}
  <div class="columns" aria-hidden="true">
    <span>State</span><span>Name</span><span>Repository</span><span>Disk</span
    ><span class="waiting">Waiting</span><span></span>
  </div>
  <ul class="rows" aria-label="Workspaces">
    {#each workspaces.list as workspace (workspace.id)}
      <li>
        <WorkspaceRow
          {workspace}
          {now}
          waiting={waitingBy[workspace.id] ?? 0}
          progress={workspaces.progress[workspace.id]}
          oom={workspaces.oom[workspace.id]}
          tools={workspaces.tools[workspace.id]}
          onStart={actions.start}
          onStop={actions.stop}
          onConnect={actions.connect}
          onRename={actions.openRename}
          onDelete={actions.askDelete}
          onDismiss={actions.dismissProgress}
        />
      </li>
    {/each}
  </ul>
{/if}

<style>
  .head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: var(--space-4);
    margin-bottom: var(--space-4);
  }
  .sub,
  .muted {
    margin: 0;
    color: var(--color-text-muted);
  }
  .rows,
  .columns {
    --workspace-columns: 8.5rem minmax(7rem, 1fr) minmax(8rem, 1.5fr) 7rem 4rem
      9.5rem;
  }
  .columns {
    display: grid;
    grid-template-columns: var(--workspace-columns);
    gap: var(--space-3);
    padding: var(--space-1) var(--space-3);
    border-bottom: 1px solid var(--color-border);
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .columns .waiting {
    text-align: end;
  }
  @media (max-width: 60rem) {
    .columns {
      display: none;
    }
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
  }
</style>
