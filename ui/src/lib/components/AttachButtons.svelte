<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import Plug from "@lucide/svelte/icons/plug";
  import Play from "@lucide/svelte/icons/play";
  import Square from "@lucide/svelte/icons/square";
  import { allows, isDown, type Workspace } from "#lib/workspaces/model.ts";
  import "#lib/theme/controls.css";

  let {
    workspace,
    onStart,
    onStop,
    onConnect,
  }: {
    workspace: Workspace;
    onStart: (w: Workspace) => void;
    onStop: (w: Workspace) => void;
    /** Opens the "how do you want to connect" step. */
    onConnect: (w: Workspace) => void;
  } = $props();

  const down = $derived(isDown(workspace.status));
  // A workspace being created reads "created" (no VM yet), but it cannot be started: its disk and
  // first clone are still being made. Offer nothing until that ends.
  const creating = $derived(workspace.busy === "creating");
</script>

{#if creating}
  <!-- no buttons while the workspace is created -->
{:else if down}
  <button
    type="button"
    class="btn primary"
    disabled={!allows(workspace, "start")}
    onclick={() => onStart(workspace)}
  >
    <Play aria-hidden="true" size={16} />Start
    <span class="visually-hidden">{workspace.name}</span>
  </button>
{:else}
  <button
    type="button"
    class="btn primary"
    disabled={!allows(workspace, "attach")}
    onclick={() => onConnect(workspace)}
  >
    <Plug aria-hidden="true" size={16} />Connect
    <span class="visually-hidden"> to {workspace.name}</span>
  </button>
  <button
    type="button"
    class="btn"
    disabled={!allows(workspace, "stop")}
    onclick={() => onStop(workspace)}
  >
    <Square aria-hidden="true" size={16} />Stop
    <span class="visually-hidden">{workspace.name}</span>
  </button>
{/if}
