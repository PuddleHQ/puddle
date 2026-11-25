<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import Toast from "#lib/components/Toast.svelte";
  import WorkspaceDialogs from "#lib/components/WorkspaceDialogs.svelte";
  import { pending } from "#lib/stores/pending.svelte.ts";
  import { workspaceActions } from "#lib/stores/workspace-actions.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";

  let { children } = $props();

  // The list itself is read by the shell (the sidebar counts it); this section adds what its
  // screens need on top: the requests waiting and the toast for an ended operation.
  onMount(() => {
    workspaces.onSettled = workspaceActions.settled;
    const stopPending = pending.start();
    return () => {
      workspaces.onSettled = null;
      stopPending();
    };
  });
</script>

{@render children()}
<WorkspaceDialogs />
<Toast />
