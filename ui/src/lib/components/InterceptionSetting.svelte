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
    problem = null;
    saved = null;
    const result = await store.setMode(select.value as WorkspaceMode);
    if (result.ok) {
      saved = "Interception mode saved.";
    } else {
      problem = result.message;
      select.value = store.mode;
    }
  }
</script>

<section class="card" aria-labelledby={headingId}>
  <h2 id={headingId}>Which hosts to decrypt</h2>
  {#if store.status === "loading"}
    <p class="muted">Loading&hellip;</p>
  {:else if store.status === "failed"}
    <div role="alert">
      <p class="muted">Couldn't read this setting.</p>
      {#if store.reason}<p class="error">{store.reason}</p>{/if}
    </div>
  {:else}
    <div class="setting">
      <div class="grow">
        <label for="ws-interception-mode">Interception mode</label>
        <p class="desc" id="ws-interception-mode-desc">
          {WARNINGS[store.effectiveMode]}
          Connections that aren't HTTPS, servers that ask for a client certificate
          and servers whose certificate Puddle doesn't accept go through untouched.
          Hosts with a credential, secret, login capture or rule on method and path
          are always decrypted. It applies at once, no restart.
        </p>
      </div>
      <select
        id="ws-interception-mode"
        aria-describedby="ws-interception-mode-desc"
        value={store.mode}
        onchange={(e) => void choose(e.currentTarget)}
      >
        <option value="inherit">
          Use the global setting ({NAMES[store.effectiveMode]})
        </option>
        <option value="all">{NAMES.all}</option>
        <option value="all_except_curated">{NAMES.all_except_curated}</option>
        <option value="known_only">{NAMES.known_only}</option>
      </select>
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
        <a href="/settings#s-net">global settings</a>):
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
  .setting {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  label {
    font-weight: 600;
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
  @media (max-width: 760px) {
    .setting {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
