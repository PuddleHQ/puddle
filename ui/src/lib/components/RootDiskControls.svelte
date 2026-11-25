<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import ConfirmDialog from "#lib/components/ConfirmDialog.svelte";
  import GrowRootDiskDialog from "#lib/components/GrowRootDiskDialog.svelte";
  import { workspaceActions as actions } from "#lib/stores/workspace-actions.svelte.ts";
  import { allows, type Workspace } from "#lib/workspaces/model.ts";
  import "#lib/theme/controls.css";

  // The buttons that change a workspace's disk, with their two dialogs. A button that cannot work
  // right now is greyed and the line under the row says why. `reclaim` adds Reclaim space (the
  // warning of a nearly full root disk offers only the two that fix that).
  let {
    workspace,
    reclaim = true,
    reset = true,
  }: { workspace: Workspace; reclaim?: boolean; reset?: boolean } = $props();

  // The grow form starts from the current size, so it waits until the runtime reports one.
  let growOpen = $state(false);
  let resetOpen = $state(false);

  const why = $derived.by(() => {
    if (workspace.busy !== null) return "Wait for what the workspace is doing.";
    if (reset && !allows(workspace, "reset_root_disk")) {
      return "Stop the workspace to reset its root disk.";
    }
    return null;
  });
</script>

<div class="row">
  <button
    type="button"
    class="btn"
    disabled={!allows(workspace, "grow_root_disk") ||
      workspace.root_disk_size_mib === null}
    onclick={() => (growOpen = true)}>Make root disk bigger…</button
  >
  {#if reclaim}
    <button
      type="button"
      class="btn"
      disabled={!allows(workspace, "reclaim")}
      onclick={() => void actions.reclaim(workspace)}>Reclaim space</button
    >
  {/if}
  {#if reset}
    <button
      type="button"
      class="btn"
      disabled={!allows(workspace, "reset_root_disk")}
      onclick={() => (resetOpen = true)}>Reset root disk…</button
    >
  {/if}
</div>
{#if why}<p class="why">{why}</p>{/if}

<GrowRootDiskDialog
  bind:open={growOpen}
  {workspace}
  onSubmit={(gib) => actions.growRootDisk(workspace, gib)}
/>
<ConfirmDialog
  bind:open={resetOpen}
  title="Reset the root disk of {workspace.name}?"
  summary="Everything installed or cached outside the volume is removed."
  detail="Packages and tools installed outside the volume, the home directory and tool caches are lost, including any file kept there that is not in a repository on the volume; packages reinstall and caches rebuild on the next start. The volume, with your repositories and files, is untouched."
  confirmLabel="Reset root disk"
  tone="deny"
  onConfirm={() => void actions.resetRootDisk(workspace)}
/>

<style>
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
</style>
