<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // The Restore part of Settings: choose a backup file, read what a restore would do on this
  // machine, choose the mode, confirm twice, and see what happened after the restart. The safety
  // copies a restore makes are listed here with their sizes; only the user removes one.
  import { onMount } from "svelte";
  import ConfirmDialog from "#lib/components/ConfirmDialog.svelte";
  import { sizeText } from "#lib/settings/backup.ts";
  import {
    applyRestore,
    deleteSafetyCopy,
    differing,
    dismissResult,
    droppedByReplace,
    loadResult,
    loadSafetyCopies,
    partLabel,
    previewRestore,
    safetyCopyTime,
    toAdd,
    type RestoreMode,
    type RestorePreview,
    type RestoreResult,
    type SafetyCopy,
  } from "#lib/settings/restore.ts";
  import "#lib/theme/controls.css";

  let picker = $state<HTMLInputElement | null>(null);
  let reading = $state(false);
  let problem = $state<string | null>(null);
  let preview = $state<RestorePreview | null>(null);
  let mode = $state<RestoreMode>("add");
  let confirming = $state(false);
  let applying = $state(false);
  let applyError = $state<string | null>(null);
  let waiting = $state<string | null>(null);
  let result = $state<RestoreResult | null>(null);
  let copies = $state<SafetyCopy[]>([]);
  let copiesProblem = $state<string | null>(null);
  let deleting = $state<SafetyCopy | null>(null);
  let deleteError = $state<string | null>(null);

  async function refresh() {
    const found = await loadResult();
    if (found.ok) result = found.value;
    const list = await loadSafetyCopies();
    if (list.ok) {
      copies = list.value;
      copiesProblem = null;
    } else copiesProblem = list.reason;
  }

  onMount(() => void refresh());

  async function choose(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;
    reading = true;
    problem = null;
    preview = null;
    waiting = null;
    const read = await previewRestore(file);
    reading = false;
    if (read.ok) {
      preview = read.value;
      mode = "add";
    } else problem = read.reason;
  }

  function cancel() {
    preview = null;
    problem = null;
    applyError = null;
  }

  async function confirm(shown: RestorePreview) {
    applying = true;
    applyError = null;
    const done = await applyRestore(shown.staging_id, mode);
    applying = false;
    if (done.ok) {
      confirming = false;
      waiting = done.value.safetyCopy;
      preview = null;
    } else applyError = done.reason;
  }

  async function dismiss() {
    const done = await dismissResult();
    if (done.ok) result = null;
  }

  async function remove(copy: SafetyCopy) {
    const done = await deleteSafetyCopy(copy.name);
    if (done.ok) {
      deleting = null;
      deleteError = null;
      await refresh();
    } else deleteError = done.reason;
  }

  function replaceSummary(shown: RestorePreview): string {
    return `Replace: ${differing(shown.parts)} entries here are replaced and ${droppedByReplace(shown.parts)} that are not in the file are dropped.`;
  }
</script>

<div class="restore">
  {#if result}
    <div
      class={result.applied ? "banner ok" : "banner error"}
      role={result.applied ? "status" : "alert"}
      data-testid="restore-result"
    >
      <p>{result.message}</p>
      <button type="button" class="btn" onclick={() => void dismiss()}
        >Dismiss</button
      >
    </div>
  {/if}

  <p class="desc">
    Restoring never changes anything until you confirm. Puddle shows what the
    file would change, keeps a safety copy of what you have now, and applies the
    file when it restarts.
  </p>
  <div class="actions">
    <input
      bind:this={picker}
      class="file"
      type="file"
      accept=".gz,.tgz,application/gzip,application/x-gzip"
      data-testid="restore-file"
      aria-label="Backup file to restore"
      onchange={(e) => void choose(e)}
    />
    <button
      type="button"
      class="btn"
      data-testid="restore-choose"
      disabled={reading || applying}
      onclick={() => picker?.click()}
    >
      {reading ? "Reading the file…" : "Restore from a file…"}
    </button>
  </div>

  {#if problem}
    <p class="error" role="alert" data-testid="restore-problem">
      This file cannot be restored: {problem}
    </p>
  {/if}

  {#if waiting}
    <p class="ok" role="status" data-testid="restore-waiting">
      Puddle is restarting to apply the file. Your previous state is kept in the
      safety copy {safetyCopyTime(waiting)}.
    </p>
  {/if}

  {#if preview}
    <section
      class="preview"
      data-testid="restore-preview"
      aria-label="What a restore would do"
    >
      <h3>What this file would do</h3>
      <p class="desc">
        Made by Puddle {preview.puddle_version ??
          "(version not recorded)"}{#if preview.created_at_ms}
          on {new Date(preview.created_at_ms).toLocaleString()}{/if}.
        {toAdd(preview.parts)} entries are new to this machine.
      </p>
      <table>
        <thead>
          <tr>
            <th scope="col">Part</th>
            <th scope="col">In the file</th>
            <th scope="col">New here</th>
            <th scope="col">Differ</th>
            <th scope="col">Same</th>
            <th scope="col">Only here</th>
          </tr>
        </thead>
        <tbody>
          {#each preview.parts as part (part.name)}
            <tr data-testid={`restore-part-${part.name}`}>
              <th scope="row">{partLabel(part.name)}</th>
              <td>{part.in_backup}</td>
              <td>{part.add}</td>
              <td>{part.differ}</td>
              <td>{part.same}</td>
              <td>{part.only_here}</td>
            </tr>
          {/each}
        </tbody>
      </table>

      {#if preview.workspaces.some((w) => w.disk === "missing")}
        <p data-testid="restore-missing-disks">
          Workspaces whose disk is not on this machine are listed as "disk
          missing" and not started:
          {preview.workspaces
            .filter((w) => w.disk === "missing")
            .map((w) => w.name)
            .join(", ")}.
        </p>
      {/if}
      {#if preview.secrets_needing_value.length > 0}
        <p data-testid="restore-secrets">
          These secrets need a value on this machine (a backup never holds one):
          {preview.secrets_needing_value.map((s) => s.name).join(", ")}.
        </p>
      {/if}
      {#if preview.running_workspaces > 0}
        <p data-testid="restore-running">
          {preview.running_workspaces} workspace{preview.running_workspaces ===
          1
            ? ""
            : "s"} running will stop while Puddle restarts.
        </p>
      {/if}

      <fieldset class="modes">
        <legend>How to restore</legend>
        <label>
          <input
            type="radio"
            name="restore-mode"
            value="add"
            bind:group={mode}
          />
          Add what is missing
          <span class="desc"
            >Nothing here is changed or removed. {differing(preview.parts)}
            entries that differ stay as yours.</span
          >
        </label>
        <label>
          <input
            type="radio"
            name="restore-mode"
            value="replace"
            bind:group={mode}
          />
          Replace
          <span class="desc"
            >Rules, settings, workspace list, identities and environment rows
            become the file's. {droppedByReplace(preview.parts)} entries that are
            not in the file are dropped (they stay in the safety copy).</span
          >
        </label>
      </fieldset>
      <div class="actions">
        <button
          type="button"
          class="btn"
          data-testid="restore-continue"
          onclick={() => {
            applyError = null;
            confirming = true;
          }}>Continue…</button
        >
        <button type="button" class="btn" onclick={cancel}>Cancel</button>
      </div>
    </section>
  {/if}

  <h3>Safety copies</h3>
  <p class="desc">
    Before a restore changes anything, Puddle saves what you have now. Puddle
    never deletes one; restoring a safety copy undoes a restore.
  </p>
  {#if copiesProblem}
    <p class="error" role="alert">{copiesProblem}</p>
  {:else if copies.length === 0}
    <p class="desc" data-testid="safety-copies-empty">No safety copies yet.</p>
  {:else}
    <ul class="copies" data-testid="safety-copies">
      {#each copies as copy (copy.name)}
        <li data-testid={`safety-copy-${copy.name}`}>
          <span>{safetyCopyTime(copy.name)}</span>
          <span class="desc">{sizeText(copy.size_bytes)}</span>
          <button
            type="button"
            class="btn"
            onclick={() => {
              deleteError = null;
              deleting = copy;
            }}>Delete</button
          >
        </li>
      {/each}
    </ul>
  {/if}
</div>

{#if preview}
  {@const shown = preview}
  <ConfirmDialog
    bind:open={confirming}
    title="Restore from this file?"
    summary={mode === "add"
      ? "Add what is missing from the file."
      : "Replace what you have with the file."}
    detail={mode === "add"
      ? "Nothing you have is changed or removed. Puddle restarts to apply it."
      : `${replaceSummary(shown)} Puddle restarts to apply it.`}
    confirmLabel={mode === "add" ? "Add and restart" : "Replace and restart"}
    tone={mode === "add" ? "allow" : "deny"}
    working={applying}
    error={applyError}
    holdOpen
    onConfirm={() => void confirm(shown)}
    onCancel={() => (applyError = null)}
  />
{/if}

{#if deleting}
  {@const copy = deleting}
  <ConfirmDialog
    open
    title="Delete this safety copy?"
    summary={`The copy of ${safetyCopyTime(copy.name)} (${sizeText(copy.size_bytes)}) is deleted.`}
    detail="Once it is gone, that restore cannot be undone from it."
    confirmLabel="Delete the copy"
    tone="deny"
    error={deleteError}
    holdOpen
    onConfirm={() => void remove(copy)}
    onCancel={() => (deleting = null)}
  />
{/if}

<style>
  .restore {
    display: grid;
    gap: var(--space-3);
  }
  p,
  h3 {
    margin: 0;
  }
  h3 {
    font-size: var(--text-md);
  }
  .desc {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .error {
    color: var(--color-danger);
  }
  .ok {
    color: var(--color-success);
  }
  .actions {
    display: flex;
    gap: var(--space-3);
  }
  .file {
    display: none;
  }
  .banner {
    display: flex;
    gap: var(--space-3);
    align-items: center;
    justify-content: space-between;
  }
  .banner.ok {
    color: var(--color-success);
  }
  .banner.error {
    color: var(--color-danger);
  }
  .preview {
    display: grid;
    gap: var(--space-3);
  }
  table {
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  th,
  td {
    text-align: left;
    padding: var(--space-1) var(--space-3);
  }
  .modes {
    display: grid;
    gap: var(--space-2);
    border: 0;
    padding: 0;
  }
  .modes label {
    display: grid;
    gap: var(--space-1);
  }
  .copies {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--space-2);
  }
  .copies li {
    display: flex;
    gap: var(--space-3);
    align-items: center;
  }
</style>
