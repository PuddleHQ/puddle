<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { LearnedLimitsStore } from "#lib/stores/learned.svelte.ts";
  import "#lib/theme/controls.css";

  // How long Puddle leaves a host alone after a tool refused its certificate, and how many such
  // hosts a workspace remembers. Workspaces that decrypt every host apply a change at once.
  const store = new LearnedLimitsStore();
  const id = $props.id();
  let minutes = $state("");
  let cap = $state("");
  let failure = $state<string | null>(null);
  let saved = $state(false);
  let working = $state(false);
  // The boxes follow what is stored until typed over.
  let seen: typeof store.limits | null = null;

  $effect(() => {
    void store.load();
  });
  $effect(() => {
    if (store.status === "ready" && store.limits !== seen) {
      seen = store.limits;
      minutes = String(Math.ceil(store.limits.ttl_secs / 60));
      cap = String(store.limits.cap);
    }
  });

  /** The boxes differ from what is stored: only then is there something to save. */
  const changed = $derived(
    store.status === "ready" &&
      (minutes !== String(Math.ceil(store.limits.ttl_secs / 60)) ||
        cap !== String(store.limits.cap)),
  );

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    working = true;
    failure = null;
    saved = false;
    const result = await store.save({
      ttl_secs: Math.round(Number(minutes) * 60),
      cap: Math.round(Number(cap)),
    });
    working = false;
    if (result.ok) saved = true;
    else failure = result.message;
  }
</script>

{#if store.status === "failed"}
  <div role="alert">
    <p class="muted">Couldn't read the limits for hosts left alone.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else if store.status === "ready"}
  <form onsubmit={submit}>
    <p class="desc">
      When a tool refuses Puddle's certificate for a host, Puddle leaves that
      host alone for a while and keeps at most this many such hosts per
      workspace (the oldest goes first). Built in: 720 minutes (12 hours) and
      1024 hosts.
    </p>
    <div class="row">
      <div class="form-field">
        <label for="{id}-minutes">Leave a host alone for (minutes)</label>
        <input
          class="input-short"
          id="{id}-minutes"
          type="number"
          min="1"
          max="43200"
          step="1"
          bind:value={minutes}
          oninput={() => (saved = false)}
        />
      </div>
      <div class="form-field">
        <label for="{id}-cap">Hosts remembered per workspace</label>
        <input
          class="input-short"
          id="{id}-cap"
          type="number"
          min="1"
          max="10000"
          step="1"
          bind:value={cap}
          oninput={() => (saved = false)}
        />
      </div>
    </div>
    <div class="row">
      {#if changed}
        <button
          type="submit"
          class="btn primary"
          aria-label="Save limits for hosts left alone"
          disabled={working}>Save</button
        >
      {/if}
      <span aria-live="polite">
        {#if saved}<span class="saved">Saved.</span>{/if}
        {#if failure}<span class="error" role="alert">{failure}</span>{/if}
      </span>
    </div>
  </form>
{/if}

<style>
  form {
    display: grid;
    gap: var(--space-2);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
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
  .saved {
    color: var(--color-success);
  }
  .error {
    margin: 0;
    color: var(--color-danger);
  }
</style>
