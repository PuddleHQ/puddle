<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import {
    LEARNING_NOTE,
    MODE_NAMES,
    MODE_WARNINGS,
    MODES,
    type Mode,
  } from "#lib/interception/modes.ts";
  import { GlobalInterception } from "#lib/stores/interception.svelte.ts";
  import "#lib/theme/controls.css";

  // The mode every workspace without its own follows, whether Puddle learns from rejections, and
  // the built-in list mode 2 passes through. A running workspace applies a change at once.
  const store = new GlobalInterception();
  let problem = $state<string | null>(null);
  let saved = $state<string | null>(null);

  $effect(() => {
    void store.load();
  });

  async function choose(select: HTMLSelectElement) {
    problem = saved = null;
    const result = await store.setMode(select.value as Mode);
    if (result.ok) saved = "Interception mode saved.";
    else {
      problem = result.message;
      select.value = store.mode;
    }
  }

  async function learning(box: HTMLInputElement) {
    problem = saved = null;
    const result = await store.setLearning(box.checked);
    if (result.ok) saved = "Learning from rejections saved.";
    else {
      problem = result.message;
      box.checked = store.learn;
    }
  }
</script>

{#if store.status === "failed"}
  <div role="alert">
    <p class="muted">Couldn't read the interception setting.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else if store.status === "ready"}
  <div class="setting">
    <div class="grow">
      <label for="set-interception-mode">Which hosts to decrypt</label>
      <p class="desc" id="set-interception-mode-desc">
        {MODE_WARNINGS[store.mode]}
        Workspaces follow this unless they have a mode of their own. Hosts with a
        credential, secret, login capture or rule on method and path are always decrypted.
        {#if !store.chosen}You haven't chosen yet: until you do, Puddle decrypts
          only hosts it has a reason to read.{/if}
      </p>
    </div>
    <select
      id="set-interception-mode"
      aria-describedby="set-interception-mode-desc"
      value={store.mode}
      onchange={(e) => void choose(e.currentTarget)}
    >
      {#each MODES as mode (mode)}
        <option value={mode}>{MODE_NAMES[mode]}</option>
      {/each}
    </select>
  </div>
  <div class="setting">
    <div class="grow">
      <label for="set-interception-learn">Learn from rejections</label>
      <p class="desc">
        {LEARNING_NOTE} It only matters in the modes that decrypt unlisted hosts.
      </p>
    </div>
    <input
      id="set-interception-learn"
      type="checkbox"
      checked={store.learn}
      onchange={(e) => void learning(e.currentTarget)}
    />
  </div>
  <div class="curated">
    <h3>
      Passed through in "except a known list" <span class="chip"
        >System managed</span
      >
    </h3>
    {#if store.curated.length === 0}
      <p class="desc">
        The list is empty for now. Puddle adds a host here once it is known to
        refuse its certificate; until then this mode behaves like decrypting
        every host.
      </p>
    {:else}
      <ul class="names">
        {#each store.curated as name (name)}<li>{name}</li>{/each}
      </ul>
    {/if}
  </div>
  <div class="status" aria-live="polite">
    {#if saved}<p class="saved">{saved}</p>{/if}
    {#if problem}<p class="error" role="alert">{problem}</p>{/if}
  </div>
{/if}

<style>
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
  /* The page's scoped select rule doesn't reach this component; without a cap the longest mode
     name sets the select's width and pushes the settings page past a phone's width. */
  select {
    min-height: var(--control-size);
    max-width: 18rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font: inherit;
  }
  input[type="checkbox"] {
    width: 1.5rem;
    height: 1.5rem;
    flex: none;
  }
  .desc,
  .muted {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  h3 {
    margin: 0;
    font-size: var(--text-md);
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
    font-weight: 400;
    color: var(--color-text-muted);
  }
  .names {
    margin: var(--space-1) 0 0;
    padding-left: var(--space-4);
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
