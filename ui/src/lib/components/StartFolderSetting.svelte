<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { untrack } from "svelte";
  import { api } from "#lib/api/client.ts";
  import { sentence } from "#lib/rules/model.ts";
  import { live } from "#lib/stores/live.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import type { Workspace } from "#lib/workspaces/model.ts";
  import { startFolderProblemOf } from "#lib/workspaces/problem-links.ts";
  import { startFolderProblem } from "#lib/workspaces/repo-rows.ts";
  import "#lib/theme/controls.css";

  // The workspace's start folder: where new terminals land and what the editor opens. It is
  // saved on its own, the folder may not exist yet, and a running workspace says whether it does.
  let { workspace }: { workspace: Workspace } = $props();

  const id = $props.id();
  const name = $derived(workspace.name);
  const stored = $derived(workspace.start_folder ?? "");
  let text = $state(untrack(() => workspace.start_folder ?? ""));
  let touched = $state(false);
  let saving = $state(false);
  let refusal = $state<string | null>(null);
  let saved = $state<string | null>(null);
  /** Whether the folder exists, as the last save or the host's problem list says; `null` unknown. */
  let exists = $state<boolean | null>(null);

  // Follows the stored value until the user starts typing: another client's change shows here.
  let seen = untrack(() => stored);
  $effect(() => {
    const now = stored;
    if (now === seen) return;
    seen = now;
    if (!untrack(() => touched)) text = now;
  });

  async function readProblem() {
    try {
      const { data } = await api.GET("/api/problems");
      if (!data) return;
      if (startFolderProblemOf(data.problems, name)) exists = false;
      else if (exists === false) exists = null;
    } catch {
      // The line below just stays as it was.
    }
  }

  // The host's problem about a missing folder is the source for "does not exist" after a reload,
  // and it follows the workspace's state (the host looks again at every start).
  const status = $derived(workspace.status);
  $effect(() => {
    void status;
    void stored;
    if (name) void readProblem();
  });

  // A problem the host raises or ends while the page is open.
  $effect(() =>
    live.subscribe({
      event: (event) => {
        if ((event as { type?: unknown }).type === "problems_changed")
          void readProblem();
      },
      resync: () => void readProblem(),
    }),
  );

  const typed = $derived(text.trim());
  const typedProblem = $derived(touched ? startFolderProblem(text) : null);
  const shown = $derived(typedProblem ?? refusal);

  async function save(event: SubmitEvent) {
    event.preventDefault();
    touched = true;
    refusal = null;
    saved = null;
    if (startFolderProblem(text)) {
      document.getElementById(`${id}-folder`)?.focus();
      return;
    }
    saving = true;
    try {
      const { data, error } = await api.PUT(
        "/api/workspaces/{id}/start-folder",
        {
          params: { path: { id: workspace.id } },
          body: { path: typed === "" ? null : typed },
        },
      );
      if (!data) {
        refusal = sentence(
          error?.message ?? "Puddle refused to change the start folder",
        );
        document.getElementById(`${id}-folder`)?.focus();
        return;
      }
      exists = data.exists;
      saved = "Start folder saved.";
      touched = false;
      await workspaces.refresh();
    } catch {
      refusal = "Puddle's service isn't answering.";
    } finally {
      saving = false;
    }
  }

  const where = $derived(
    stored === "" ? "the workspace root" : `/workspaces/${name}/${stored}`,
  );
</script>

<section class="card" id="start-folder" aria-labelledby="{id}-h">
  <h2 id="{id}-h">Start folder</h2>
  <form onsubmit={save} novalidate>
    <div class="field">
      <label for="{id}-folder">Start folder</label>
      <input
        id="{id}-folder"
        type="text"
        autocomplete="off"
        spellcheck="false"
        placeholder="Empty: the workspace root"
        bind:value={text}
        oninput={() => {
          touched = true;
          refusal = null;
          saved = null;
        }}
        aria-invalid={shown ? "true" : undefined}
        aria-describedby={shown ? `${id}-error` : `${id}-hint`}
      />
      {#if shown}
        <p class="error" id="{id}-error" role="alert">{shown}</p>
      {:else}
        <p class="desc" id="{id}-hint">
          A path under the workspace root, such as <span class="mono"
            >api/services/web</span
          >. Leave it empty for the root itself. The folder may not exist yet.
        </p>
      {/if}
    </div>
    <div class="row">
      <button type="submit" class="btn primary" disabled={saving}>Save</button>
    </div>
  </form>
  <div aria-live="polite">
    {#if saved}<p class="saved">{saved}</p>{/if}
    {#if stored !== "" && exists === true}
      <p class="desc">The folder exists in the workspace.</p>
    {:else if stored !== "" && exists === false}
      <p class="warning">
        <b>The folder does not exist.</b> New terminals and the editor open the workspace
        root until it does. Create it in the workspace, or change the start folder
        here.
      </p>
    {:else if stored !== "" && workspace.status !== "running"}
      <p class="desc">Puddle looks for the folder when the workspace starts.</p>
    {/if}
  </div>
  <p class="desc">
    New terminals land in {where}, and the editor opens it. Several checkouts at
    the workspace root open together in the editor.
  </p>
</section>

<style>
  .card {
    display: grid;
    gap: var(--space-3);
    max-width: 48rem;
    margin-bottom: var(--space-4);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  h2 {
    font-size: var(--text-md);
  }
  form {
    display: grid;
    gap: var(--space-3);
  }
  .field {
    display: grid;
    gap: var(--space-1);
  }
  label {
    font-weight: 600;
  }
  input {
    min-height: var(--control-size);
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font: inherit;
  }
  input[aria-invalid="true"] {
    border-color: var(--color-danger);
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
  .warning {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
    overflow-wrap: anywhere;
  }
  .row {
    display: flex;
    gap: var(--space-2);
  }
</style>
