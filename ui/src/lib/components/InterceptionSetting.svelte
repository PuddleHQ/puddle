<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onLearnedChanged } from "#lib/interception/learned.ts";
  import LearnedHostsList from "#lib/components/LearnedHostsList.svelte";
  import NeverDecryptEditor from "#lib/components/NeverDecryptEditor.svelte";
  import {
    MODE_NAMES as NAMES,
    MODE_WARNINGS as WARNINGS,
  } from "#lib/interception/modes.ts";
  import {
    WorkspaceInterception,
    type WorkspaceMode,
  } from "#lib/stores/interception.svelte.ts";
  import "#lib/theme/controls.css";

  // Which HTTPS hosts of one workspace Puddle decrypts beyond the ones with a credential, and the
  // names it leaves alone. The workspace applies a change at once.
  let { id }: { id: string } = $props();

  const store = new WorkspaceInterception();
  const headingId = $props.id();
  let problem = $state<string | null>(null);
  let saved = $state<string | null>(null);

  $effect(() => {
    void store.load(id);
    // Making a learned host permanent changes the names above the list.
    return onLearnedChanged((workspace) => {
      if (workspace === id) void store.load(id, true);
    });
  });

  async function choose(select: HTMLSelectElement) {
    await setMode(select.value as WorkspaceMode, () => {
      select.value = store.effectiveMode;
    });
  }

  const useGlobal = () => setMode("inherit", () => {});

  async function setMode(mode: WorkspaceMode, undo: () => void) {
    problem = null;
    saved = null;
    const result = await store.setMode(mode);
    if (result.ok) {
      saved = "Interception mode saved.";
    } else {
      problem = result.message;
      undo();
    }
  }
</script>

<section class="card" aria-labelledby={headingId}>
  <h3 id={headingId}>Which hosts to decrypt</h3>
  {#if store.status === "loading"}
    <p class="muted">Loading&hellip;</p>
  {:else if store.status === "failed"}
    <div role="alert">
      <p class="muted">Couldn't read this setting.</p>
      {#if store.reason}<p class="error">{store.reason}</p>{/if}
    </div>
  {:else}
    <div class="setting">
      <select
        id="ws-interception-mode"
        aria-labelledby={headingId}
        aria-describedby="ws-interception-mode-desc"
        value={store.effectiveMode}
        onchange={(e) => void choose(e.currentTarget)}
      >
        <option value="all">{NAMES.all}</option>
        <option value="all_except_curated">{NAMES.all_except_curated}</option>
        <option value="known_only">{NAMES.known_only}</option>
      </select>
      <p class="desc" id="ws-interception-mode-desc">
        {WARNINGS[store.effectiveMode]}
        Connections that aren't HTTPS, servers that ask for a client certificate and
        servers whose certificate Puddle doesn't accept go through untouched. Hosts
        with a credential, secret, login capture or rule on method and path are always
        decrypted. It applies at once, no restart.
      </p>
      <p class="from">
        <span class="meta"
          >{store.mode === "inherit"
            ? "Global setting"
            : "This workspace"}</span
        >
        {#if store.mode !== "inherit"}
          <button
            type="button"
            class="reset"
            aria-label="Use the global setting again for Which hosts to decrypt"
            onclick={() => void useGlobal()}>Use global</button
          >
        {/if}
      </p>
    </div>
    <div class="status" aria-live="polite">
      {#if saved}<p class="saved">{saved}</p>{/if}
      {#if problem}<p class="error" role="alert">{problem}</p>{/if}
    </div>
    <NeverDecryptEditor
      label="Never decrypt in this workspace"
      hint="One host name per line, or *. and a domain such as *.example.com. These stay untouched while the mode decrypts unlisted hosts. Hosts with a credential are always decrypted."
      names={store.names}
      save={(names) => store.saveNames(names)}
    />
    {#if store.globalNames.length > 0}
      <p class="desc">
        Never decrypted in any workspace (set in
        <a href="/settings/network#set-interception-mode">global settings</a>):
        {store.globalNames.join(", ")}
      </p>
    {/if}
    <LearnedHostsList {id} />
  {/if}
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
  .desc,
  .muted {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .status p {
    margin: 0;
  }
  .saved {
    color: var(--color-success);
  }
  .error {
    color: var(--color-danger);
  }
  .from {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin: 0;
    font-size: var(--text-sm);
  }
  .reset {
    padding: 0;
    border: 0;
    background: none;
    color: var(--color-accent);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
</style>
