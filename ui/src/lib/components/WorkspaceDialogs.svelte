<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import ConnectDialog from "./ConnectDialog.svelte";
  import CreateWorkspaceDialog from "./CreateWorkspaceDialog.svelte";
  import DeleteWorkspaceDialog from "./DeleteWorkspaceDialog.svelte";
  import DirectSshDialog from "./DirectSshDialog.svelte";
  import RenameWorkspaceDialog from "./RenameWorkspaceDialog.svelte";
  import { editorProblems } from "#lib/stores/editor-problems.svelte.ts";
  import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
  import { workspaceActions as actions } from "#lib/stores/workspace-actions.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import { serverOf } from "#lib/settings/model.ts";
  import { trustWordsFor } from "#lib/workspaces/direct-ssh.ts";

  // The connect step shows the live workspace, so the switch follows what is stored.
  const connecting = $derived(
    actions.connectOpen && actions.connectFor
      ? (workspaces.list.find((w) => w.id === actions.connectFor?.id) ??
          actions.connectFor)
      : null,
  );
  // The rename dialog shows the live workspace, so a clash with a name set elsewhere shows at once.
  const renaming = $derived(
    actions.renameOpen && actions.renaming
      ? (workspaces.list.find((w) => w.id === actions.renaming?.id) ??
          actions.renaming)
      : null,
  );
  const server = $derived(
    globalSettings.view ? serverOf(globalSettings.view) : null,
  );

  // The server's name comes from the global settings; read them when the step opens.
  $effect(() => {
    if (actions.connectOpen) void globalSettings.load(true);
  });
  // Whether the editor server could be installed is read while the step is open.
  $effect(() => {
    if (actions.connectOpen) return editorProblems.start();
  });
</script>

<CreateWorkspaceDialog bind:open={actions.createOpen} />

{#if connecting}
  <ConnectDialog
    bind:open={actions.connectOpen}
    workspace={connecting}
    {server}
    editorProblem={editorProblems.of(connecting.name)}
    onBrowser={actions.openBrowser}
    onDesktop={actions.openDesktop}
    onDirectSsh={(w, on) => actions.requestDirectSsh(w, on, true)}
  />
{/if}

<!-- The trust text, said once when direct SSH is turned on for a workspace. -->
<DirectSshDialog
  bind:open={actions.trustOpen}
  words={trustWordsFor(actions.trustFor?.display_name ?? "this workspace")}
  onConfirm={actions.confirmTrust}
  onCancel={actions.cancelTrust}
/>

{#if renaming}
  <RenameWorkspaceDialog
    bind:open={actions.renameOpen}
    workspace={renaming}
    all={workspaces.list}
    onSubmit={(text) => actions.submitRename(renaming, text)}
  />
{/if}

{#if actions.deleting}
  <DeleteWorkspaceDialog
    bind:open={actions.deleteOpen}
    name={actions.deleting.workspace.display_name}
    check={actions.deleting.check}
    removes={actions.deleting.removes}
    working={actions.deleting.working}
    error={actions.deleting.error}
    onConfirm={actions.confirmDelete}
    onCancel={() => {}}
  />
{/if}
