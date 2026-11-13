<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { howLongAgo, refusals, timeLeft } from "#lib/interception/learned.ts";
  import { LearnedList } from "#lib/stores/learned.svelte.ts";
  import "#lib/theme/controls.css";

  // The hosts a tool in this workspace refused Puddle's certificate for: how often and when, whether
  // Puddle leaves it alone now, and the buttons that try decrypting it again or make it permanent.
  // The list follows the live stream.
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
  <h3 id={headingId}>Hosts a tool refused Puddle's certificate for</h3>
  {#if store.status === "failed"}
    <div role="alert">
      <p class="muted">Couldn't read this list.</p>
      {#if store.reason}<p class="error">{store.reason}</p>{/if}
    </div>
  {:else if store.status === "ready"}
    {#if store.hosts.length === 0}
      <p class="muted">
        None. A host appears here when a tool in this workspace refuses Puddle's
        certificate for it. The history is kept; Puddle leaves a host alone only
        while the workspace runs.
      </p>
    {:else}
      <ul>
        {#each store.hosts as learned (learned.host)}
          <li>
            <span class="host">{learned.host}</span>
            <span class="muted">
              {refusals(learned.count)}, first {howLongAgo(
                learned.first_rejected_at,
              )}, last {howLongAgo(learned.last_rejected_at)};
              {#if learned.expires_in_secs !== null}
                left alone, decrypted again by itself in {timeLeft(
                  learned.expires_in_secs,
                )}
              {:else}
                decrypted
              {/if}
            </span>
            <span class="buttons">
              {#if learned.passed_through}
                <button
                  type="button"
                  class="btn"
                  disabled={working}
                  aria-label="Try decrypting {learned.host} again"
                  onclick={() => run(() => store.decryptAgain(learned.host))}
                >
                  Try decrypting again
                </button>
              {/if}
              <button
                type="button"
                class="btn"
                disabled={working}
                aria-label="Never decrypt {learned.host} in this workspace"
                onclick={() =>
                  run(() => store.makePermanent(learned.host, "workspace"))}
              >
                Make permanent here
              </button>
              <button
                type="button"
                class="btn"
                disabled={working}
                aria-label="Never decrypt {learned.host} in any workspace"
                onclick={() =>
                  run(() => store.makePermanent(learned.host, "global"))}
              >
                Make permanent everywhere
              </button>
            </span>
          </li>
        {/each}
      </ul>
      {#if store.hosts.some((h) => h.passed_through)}
        <div class="row">
          <button
            type="button"
            class="btn"
            disabled={working}
            onclick={() => run(() => store.decryptAll())}
          >
            Try decrypting all again
          </button>
        </div>
      {/if}
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
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
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
