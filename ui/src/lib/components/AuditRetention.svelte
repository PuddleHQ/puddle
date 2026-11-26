<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { AuditRetentionStore } from "#lib/stores/audit-retention.svelte.ts";
  import {
    growthLabel,
    largeWarning,
    oldestLabel,
    parseDays,
    perDayLabel,
    recordsLabel,
  } from "#lib/audit/retention.ts";
  import { bytesLabel } from "#lib/audit/model.ts";
  import "#lib/theme/controls.css";

  // The size of the activity log and how many days it is kept. The owner sees it: the log covers
  // every user's records.
  let { store = new AuditRetentionStore() }: { store?: AuditRetentionStore } =
    $props();
  const id = $props.id();
  let days = $state("");
  let failure = $state<string | null>(null);
  let saved = $state(false);
  let working = $state(false);
  // The box follows what is stored until typed over.
  let seen: typeof store.size = null;

  $effect(() => {
    void store.load();
  });
  $effect(() => {
    if (store.size && store.size !== seen) {
      seen = store.size;
      days = String(store.size.retention_days);
    }
  });

  /** The box differs from what is stored: only then is there something to save. */
  const changed = $derived(
    store.size !== null && days !== String(store.size.retention_days),
  );

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    saved = false;
    const parsed = parseDays(days);
    if (!parsed.ok) {
      failure = parsed.message;
      return;
    }
    working = true;
    failure = null;
    const result = await store.save(parsed.days);
    working = false;
    if (result.ok) saved = true;
    else failure = result.message;
  }
</script>

{#if store.status === "failed"}
  <div role="alert">
    <p class="muted">Couldn't read the size of the activity log.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else if store.size}
  {@const size = store.size}
  {@const warning = largeWarning(size)}
  {#if warning}<p class="warning" role="status">{warning}</p>{/if}
  <dl>
    <div>
      <dt>Records</dt>
      <dd>{recordsLabel(size.records)}, {bytesLabel(size.bytes)}</dd>
    </div>
    <div>
      <dt>Oldest record</dt>
      <dd>{oldestLabel(size, Date.now())}</dd>
    </div>
    <div>
      <dt>Last 24 hours</dt>
      <dd>{growthLabel(size.records_last_day, size.bytes_last_day)}</dd>
    </div>
    <div>
      <dt>Last 7 days</dt>
      <dd>
        {growthLabel(size.records_last_week, size.bytes_last_week)} ({perDayLabel(
          size,
        )})
      </dd>
    </div>
    <div>
      <dt>Database file</dt>
      <dd>
        {bytesLabel(size.file_bytes)}, {bytesLabel(size.free_bytes)} of it free
      </dd>
    </div>
  </dl>
  <form onsubmit={submit} novalidate>
    <p class="desc">
      Puddle deletes records older than this within the hour, and they can't be
      brought back. Nothing is deleted for the log's size. Built in: 90 days.
    </p>
    <div class="row">
      <label for="{id}-days">Keep the activity log for (days)</label>
      <input
        class="input-short"
        id="{id}-days"
        type="number"
        min="1"
        max="3650"
        step="1"
        value={days}
        oninput={(event) => {
          days = event.currentTarget.value;
          saved = false;
        }}
      />
      {#if changed}
        <button
          type="submit"
          class="btn primary"
          aria-label="Save how long the activity log is kept"
          disabled={working}
        >
          Save
        </button>
      {/if}
      <span aria-live="polite">
        {#if saved}<span class="saved">Saved.</span>{/if}
        {#if failure}<span class="error" role="alert">{failure}</span>{/if}
      </span>
    </div>
  </form>
{:else}
  <p class="muted">Reading the activity log&hellip;</p>
{/if}

<style>
  dl {
    display: grid;
    gap: var(--space-1);
    margin: 0 0 var(--space-3);
  }
  dl div {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  dt {
    min-width: 10rem;
    font-weight: 600;
  }
  dd {
    margin: 0;
  }
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
  .warning {
    margin: 0 0 var(--space-3);
    color: var(--color-warning);
    font-weight: 600;
  }
  .saved {
    color: var(--color-success);
  }
  .error {
    margin: 0;
    color: var(--color-danger);
  }
</style>
