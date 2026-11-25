<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import ConfirmDialog from "#lib/components/ConfirmDialog.svelte";
  import AllowDenyPair from "#lib/components/AllowDenyPair.svelte";
  import DecisionFlow from "#lib/components/DecisionFlow.svelte";
  import LoadNotice from "#lib/components/LoadNotice.svelte";
  import RequestRow from "#lib/components/RequestRow.svelte";
  import { narrowest, type Effect } from "#lib/decision/model.ts";
  import { pending } from "#lib/stores/pending.svelte.ts";
  import { ruleSets } from "#lib/stores/rule-sets.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import "#lib/theme/controls.css";

  // The requests one workspace is waiting on, with Allow and Deny on each and, from two on,
  // Allow all and Deny all. A bulk choice is the narrowest one for each request (this workspace,
  // this exact host, permanent), the same as a click on Allow or Deny.
  let { workspace, name }: { workspace: string; name: string } = $props();

  let now = $state(Date.now());
  let currentId = $state<number | null>(null);
  let heading = $state<HTMLElement>();
  let flow = $state<ReturnType<typeof DecisionFlow>>();
  let bulk = $state<Effect>("allow");
  let bulkOpen = $state(false);
  let bulkCount = $state(0);
  let working = $state(false);

  onMount(() => {
    const stopSets = ruleSets.start();
    const clock = setInterval(() => {
      now = Date.now();
    }, 30_000);
    return () => {
      stopSets();
      clearInterval(clock);
    };
  });

  const rows = $derived(
    pending.rows
      .filter((r) => r.request.workspace === workspace)
      .sort(
        (a, b) =>
          b.request.first_seen - a.request.first_seen ||
          b.request.id - a.request.id,
      ),
  );

  function ask(effect: Effect) {
    bulk = effect;
    bulkCount = rows.length;
    bulkOpen = true;
  }

  async function decideAll() {
    const effect = bulk;
    working = true;
    let done = 0;
    let failed = 0;
    for (const row of [...rows]) {
      const result = await pending.decide(row, narrowest(effect), false);
      if (result.ok) done += 1;
      else failed += 1;
    }
    working = false;
    bulkOpen = false;
    const verb = effect === "allow" ? "Allowed" : "Denied";
    const what = (n: number) => `${n} ${n === 1 ? "request" : "requests"}`;
    toasts.push(
      failed === 0
        ? `${verb} ${what(done)} for ${name}.`
        : `${verb} ${what(done)} for ${name}; ${what(failed)} could not be decided.`,
      { tone: failed === 0 ? "info" : "error", ms: failed === 0 ? 5000 : 8000 },
    );
    heading?.focus();
  }
</script>

<section class="block" aria-labelledby="waiting-h">
  <div class="head">
    <h2 id="waiting-h" tabindex="-1" bind:this={heading}>
      Waiting for you
      {#if rows.length > 0}<span class="count">{rows.length}</span>{/if}
    </h2>
    {#if rows.length > 1}
      <div class="bulk">
        <AllowDenyPair
          allowLabel="Allow all"
          denyLabel="Deny all"
          onAllow={() => ask("allow")}
          onDeny={() => ask("deny")}
        />
      </div>
    {/if}
  </div>
  {#if pending.status === "loading"}
    <p class="muted pad">Loading requests&hellip;</p>
  {:else if pending.status === "failed"}
    <LoadNotice what="the requests" message={pending.loadError} padded />
  {:else}
    {#if pending.loadError}
      <LoadNotice
        what="the requests"
        message={pending.loadError}
        stale
        padded
      />
    {/if}
    {#if rows.length === 0}
      <p class="muted pad">Nothing is waiting for {name}.</p>
    {:else}
      <ul>
        {#each rows as row (row.request.id)}
          <RequestRow
            {row}
            {now}
            current={row.request.id === currentId}
            blockedBy={pending.blockedBy(row.request)}
            optionsOpen={flow?.optionsOpenFor(row.request.id) ?? false}
            onMore={(r, anchor) => flow?.more(r, anchor)}
            onDecide={(r, choice) => flow?.decide(r, choice)}
            onFocusRow={(r) => {
              currentId = r.request.id;
            }}
          />
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<DecisionFlow bind:this={flow} heading={() => heading} />
<ConfirmDialog
  bind:open={bulkOpen}
  title="{bulk === 'deny' ? 'Deny' : 'Allow'} all {bulkCount} requests?"
  summary="Each request of {name} gets its own rule: this workspace, that exact host, permanently."
  detail="You can take each one back on the Rules page."
  confirmLabel={bulk === "deny" ? "Deny all" : "Allow all"}
  tone={bulk === "deny" ? "deny" : "allow"}
  {working}
  holdOpen
  onConfirm={() => void decideAll()}
/>

<style>
  .block {
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    padding-inline-end: var(--space-4);
    border-bottom: 1px solid var(--color-border-subtle);
  }
  h2 {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    font-size: var(--text-md);
  }
  h2:focus {
    outline: none;
  }
  .bulk {
    display: flex;
    gap: var(--space-2);
  }
  .count {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .muted {
    margin: 0;
    color: var(--color-text-muted);
  }
  .pad {
    padding: var(--space-4);
  }
</style>
