<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { timeLeft } from "#lib/interception/learned.ts";
  import { LearnedList } from "#lib/stores/learned.svelte.ts";
  import "#lib/theme/controls.css";

  // The hosts a tool in this workspace refused Puddle's certificate for, which Puddle now leaves
  // alone, each with a button that decrypts it again. The list follows the live stream.
  let { id }: { id: string } = $props();

  const store = new LearnedList();
  const headingId = $props.id();
  let problem = $state<string | null>(null);
  let working = $state(false);

  $effect(() => store.start(id));

  async function run(call: () => Promise<{ ok: boolean; message?: string }>) {
    working = true;
    problem = null;
    const result = await call();
    working = false;
    if (!result.ok) problem = result.message ?? null;
  }
</script>

<div class="learned" role="group" aria-labelledby={headingId}>
  <h3 id={headingId}>Left alone after a tool refused Puddle's certificate</h3>
  {#if store.status === "failed"}
    <div role="alert">
      <p class="muted">Couldn't read this list.</p>
      {#if store.reason}<p class="error">{store.reason}</p>{/if}
    </div>
  {:else if store.status === "ready"}
    {#if store.hosts.length === 0}
      <p class="muted">
        None. A host appears here when a tool in this workspace refuses Puddle's
        certificate for it. The list is kept while the workspace runs.
      </p>
    {:else}
      <ul>
        {#each store.hosts as learned (learned.host)}
          <li>
            <span class="host">{learned.host}</span>
            <span class="muted">
              decrypted again by itself in {timeLeft(learned.expires_in_secs)}
            </span>
            <button
              type="button"
              class="btn"
              disabled={working}
              aria-label="Decrypt {learned.host} again"
              onclick={() => run(() => store.decryptAgain(learned.host))}
            >
              Decrypt again
            </button>
          </li>
        {/each}
      </ul>
      <div class="row">
        <button
          type="button"
          class="btn"
          disabled={working}
          onclick={() => run(() => store.decryptAll())}
        >
          Decrypt all again
        </button>
      </div>
    {/if}
    <div aria-live="polite">
      {#if problem}<p class="error" role="alert">{problem}</p>{/if}
    </div>
  {/if}
</div>

<style>
  .learned {
    display: grid;
    gap: var(--space-2);
  }
  h3 {
    margin: 0;
    font-size: var(--text-sm);
    font-weight: 600;
  }
  ul {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
  }
  .host {
    font-family: var(--font-mono);
  }
  .muted,
  .error {
    margin: 0;
    font-size: var(--text-sm);
  }
  .muted {
    color: var(--color-text-muted);
  }
  .error {
    color: var(--color-danger);
  }
</style>
