<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { goto } from "$app/navigation";
  import {
    DEFAULT_MODE,
    MODE_NAMES,
    MODE_WARNINGS,
    MODES,
    type Mode,
  } from "#lib/interception/modes.ts";
  import { GlobalInterception } from "#lib/stores/interception.svelte.ts";
  import StepHeading from "#lib/welcome/StepHeading.svelte";
  import { around, WELCOME_ROOT } from "#lib/welcome/steps.ts";
  import "#lib/theme/controls.css";

  const store = new GlobalInterception();
  let choice = $state<Mode>(DEFAULT_MODE);
  let busy = $state(false);
  let problem = $state<string | null>(null);

  $effect(() => {
    void store.load().then(() => {
      // Coming back keeps what was chosen; a first visit preselects the recommended mode.
      if (store.chosen) choice = store.mode;
    });
  });

  /** Saves the choice, then goes on. Going Back saves nothing. */
  async function carryOn() {
    busy = true;
    problem = null;
    const result = await store.setMode(choice);
    busy = false;
    if (result.ok) await goto(`${WELCOME_ROOT}/look`);
    else problem = result.message;
  }
</script>

<StepHeading
  title="Which hosts should Puddle decrypt?"
  lead="Puddle reads HTTPS traffic so a rule can allow one request and block another on the same host. You can change this later in Settings."
/>

{#if store.status === "loading"}
  <p class="muted">Loading&hellip;</p>
{:else if store.status === "failed"}
  <div role="alert">
    <p class="error">Couldn't read this setting.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else}
  <fieldset class="card">
    <legend>Interception</legend>
    {#each MODES as mode (mode)}
      <label class="choice">
        <input type="radio" name="mode" value={mode} bind:group={choice} />
        <span>
          <b>{MODE_NAMES[mode]}</b>
          {#if mode === DEFAULT_MODE}<span class="chip">recommended</span>{/if}
          <span class="desc">{MODE_WARNINGS[mode]}</span>
        </span>
      </label>
    {/each}
    <p class="desc">
      Hosts with a credential, secret or rule on method and path are always
      decrypted. When a tool refuses Puddle's certificate, Puddle passes that
      host through for a while and lists it for you.
    </p>
  </fieldset>
{/if}

<div class="status" aria-live="polite">
  {#if problem}<p class="error" role="alert">{problem}</p>{/if}
</div>
<div class="actions">
  <a class="btn" href={around("interception").back}>Back</a>
  <button
    type="button"
    class="btn primary"
    disabled={busy || store.status !== "ready"}
    onclick={() => void carryOn()}>Continue</button
  >
</div>

<style>
  .muted,
  .desc {
    color: var(--color-text-muted);
  }
  .muted {
    margin: 0;
  }
  .desc {
    display: block;
    font-size: var(--text-sm);
  }
  p.desc {
    margin: 0;
  }
  .status p {
    margin: 0;
  }
  .error {
    color: var(--color-danger);
  }
  .card {
    display: grid;
    gap: var(--space-3);
    margin: 0;
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  legend {
    padding: 0 var(--space-1);
    font-weight: 600;
  }
  .choice {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    cursor: pointer;
  }
  .choice input {
    flex: none;
    width: 1.5rem;
    height: 1.5rem;
    margin: 0;
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
