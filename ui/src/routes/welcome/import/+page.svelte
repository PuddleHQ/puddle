<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // BK-30: bring a backup onto a machine that has nothing yet. It is the same preview and apply
  // as Settings (Add and Replace give one result on an empty machine, so there is no mode choice).
  // The first-run flag is not taken over (BK-31): after the restart the flow starts again and the
  // steps about this machine, the check, the certificates and the Git host, run as usual.
  import RestorePreviewFacts from "#lib/components/RestorePreviewFacts.svelte";
  import {
    applyRestore,
    previewRestore,
    safetyCopyTime,
    type RestorePreview,
  } from "#lib/settings/restore.ts";
  import StepHeading from "#lib/welcome/StepHeading.svelte";
  import { WELCOME_ROOT } from "#lib/welcome/steps.ts";
  import "#lib/theme/controls.css";

  let picker = $state<HTMLInputElement | null>(null);
  let reading = $state(false);
  let problem = $state<string | null>(null);
  let preview = $state<RestorePreview | null>(null);
  let applying = $state(false);
  let applyError = $state<string | null>(null);
  let waiting = $state<string | null>(null);

  async function choose(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;
    reading = true;
    problem = null;
    preview = null;
    const read = await previewRestore(file);
    reading = false;
    if (read.ok) preview = read.value;
    else problem = read.reason;
  }

  async function importIt(shown: RestorePreview) {
    applying = true;
    applyError = null;
    const done = await applyRestore(shown.staging_id, "add");
    applying = false;
    if (done.ok) {
      waiting = done.value.safetyCopy;
      preview = null;
    } else applyError = done.reason;
  }
</script>

<StepHeading
  title="Import a backup"
  lead="Bring your rules, settings, workspaces and Git identities from a backup file made by Puddle."
/>

<input
  bind:this={picker}
  class="file"
  type="file"
  accept=".gz,.tgz,application/gzip,application/x-gzip"
  data-testid="import-file"
  aria-label="Backup file to import"
  onchange={(e) => void choose(e)}
/>

{#if waiting}
  <p class="ok" role="status" data-testid="import-waiting">
    Puddle is restarting to apply the file. Setup continues after the restart
    with the steps about this computer. Nothing was here before; the safety copy
    is {safetyCopyTime(waiting)}.
  </p>
{:else}
  <p class="body">
    Nothing is changed until you confirm. The checks of this computer, the
    certificates and the connection to a Git host still run afterwards.
  </p>

  {#if problem}
    <p class="error" role="alert" data-testid="import-problem">
      This file cannot be imported: {problem}
    </p>
  {/if}

  {#if preview}
    <section aria-label="What the import would do" data-testid="import-preview">
      <RestorePreviewFacts {preview} />
    </section>
    {#if applyError}
      <p class="error" role="alert" data-testid="import-error">{applyError}</p>
    {/if}
  {/if}

  <div class="actions">
    <a class="btn" href={WELCOME_ROOT}>Back</a>
    {#if preview}
      {@const shown = preview}
      <button
        type="button"
        class="btn"
        disabled={applying}
        onclick={() => (preview = null)}
      >
        Choose another file
      </button>
      <button
        type="button"
        class="btn primary"
        data-testid="import-confirm"
        disabled={applying}
        onclick={() => void importIt(shown)}
      >
        {applying ? "Importing…" : "Import and restart"}
      </button>
    {:else}
      <button
        type="button"
        class="btn primary"
        data-testid="import-choose"
        disabled={reading}
        onclick={() => picker?.click()}
      >
        {reading ? "Reading the file…" : "Choose a backup file…"}
      </button>
    {/if}
  </div>
{/if}

<style>
  .body,
  p {
    margin: 0;
  }
  .error {
    color: var(--color-danger);
  }
  .ok {
    color: var(--color-success);
  }
  .file {
    display: none;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
