<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { api } from "#lib/api/client.ts";
  import { sentence } from "#lib/format/sentence.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import type { Workspace } from "#lib/workspaces/model.ts";
  import "#lib/theme/controls.css";

  // The guest user the workspace's terminals, editor and clones run as. It is saved on its own
  // and applies at the next start: a running workspace keeps the user it started with.
  let { workspace }: { workspace: Workspace } = $props();

  type User = Workspace["session_user"];

  const id = $props.id();
  let saving = $state(false);
  let refusal = $state<string | null>(null);
  let saved = $state<string | null>(null);

  async function change(select: HTMLSelectElement) {
    const wanted = select.value as User;
    const before = workspace.session_user;
    refusal = null;
    saved = null;
    saving = true;
    try {
      const { data, error } = await api.PUT(
        "/api/workspaces/{id}/session-user",
        {
          params: { path: { id: workspace.id } },
          body: { session_user: wanted },
        },
      );
      if (!data) {
        refusal = sentence(
          error?.message ?? "Puddle refused to change the user",
        );
        select.value = before;
        return;
      }
      saved =
        workspace.status === "running"
          ? `Saved. The workspace runs as ${wanted} from its next start.`
          : `Saved. The workspace runs as ${wanted} when it starts.`;
      await workspaces.refresh();
    } catch {
      refusal = "Puddle's service isn't answering.";
      select.value = before;
    } finally {
      saving = false;
    }
  }
</script>

<section class="card" id="session-user" aria-labelledby="{id}-h">
  <h3 id="{id}-h">User</h3>
  <div class="setting">
    <div class="grow">
      <label for="{id}-user">Terminals and the editor run as</label>
      <p class="desc" id="{id}-desc">
        <span class="mono">agent</span> is a plain user that becomes root with
        <span class="mono">sudo</span> (no password;
        <span class="mono">sudo -i</span>
        for a root shell); an image without <span class="mono">sudo</span>
        leaves it no way to root, so choose root there.
        <span class="mono">root</span> is what workspaces made before ran as. A change
        takes effect when the workspace next starts, and that start gives every file
        on the workspace's disk to the new user, which takes a while on a disk with
        many files.
      </p>
    </div>
    <select
      id="{id}-user"
      aria-describedby="{id}-desc"
      value={workspace.session_user}
      disabled={saving}
      onchange={(e) => void change(e.currentTarget)}
    >
      <option value="agent">agent, with sudo</option>
      <option value="root">root</option>
    </select>
  </div>
  <div aria-live="polite">
    {#if refusal}<p class="error" role="alert">{refusal}</p>{/if}
    {#if saved}<p class="saved">{saved}</p>{/if}
  </div>
</section>

<style>
  .card {
    display: grid;
    gap: var(--space-3);
  }
  h3 {
    font-size: var(--text-md);
  }
  .setting {
    display: grid;
    gap: var(--space-2);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  label {
    font-weight: 600;
  }
  p {
    margin: 0;
  }
  .desc {
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .error {
    color: var(--color-danger);
    font-size: var(--text-sm);
  }
  .saved {
    color: var(--color-success);
  }
</style>
