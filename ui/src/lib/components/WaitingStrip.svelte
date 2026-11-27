<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import AllowDenyPair from "./AllowDenyPair.svelte";
  import DecisionFlow from "./DecisionFlow.svelte";
  import HostName from "./HostName.svelte";
  import { narrowest, type Effect } from "#lib/decision/model.ts";
  import { pending as defaultStore } from "#lib/stores/pending.svelte.ts";
  import type { PendingStore } from "#lib/stores/pending.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import "#lib/theme/controls.css";

  // The gate in view: a strip at the top of every screen. Something waiting makes it the yellow
  // "needs you" strip, with the newest request answerable on the spot (the same narrow Allow or
  // Deny as the Inbox: this workspace, this exact host, for good); nothing waiting leaves one quiet
  // grey line. On the Inbox itself the list is right below, so the strip only counts.
  let { store = defaultStore }: { store?: PendingStore } = $props();

  let flow = $state<ReturnType<typeof DecisionFlow>>();

  onMount(() => store.start());

  const inInbox = $derived(
    page.url.pathname === "/inbox" || page.url.pathname.startsWith("/inbox/"),
  );
  const newest = $derived(
    [...store.rows].sort(
      (a, b) =>
        b.request.first_seen - a.request.first_seen ||
        b.request.id - a.request.id,
    )[0],
  );
  /** Open requests per workspace, most first. */
  const perWorkspace = $derived.by(() => {
    // eslint-disable-next-line svelte/prefer-svelte-reactivity -- built and read inside one derivation
    const counts = new Map<string, number>();
    for (const { request } of store.rows) {
      counts.set(request.workspace, (counts.get(request.workspace) ?? 0) + 1);
    }
    return [...counts]
      .map(([id, n]) => ({ id, n, name: workspaces.labelOf(id) }))
      .sort((a, b) => b.n - a.n || a.name.localeCompare(b.name));
  });
  const blocked = $derived(
    newest ? store.blockedBy(newest.request) !== null : false,
  );

  function answer(effect: Effect) {
    if (newest) flow?.decide(newest, narrowest(effect));
  }
</script>

{#if store.count > 0}
  <div class="strip waiting" aria-live="polite" data-testid="waiting-strip">
    <p class="tally">
      <span class="dot" aria-hidden="true"></span>
      <b>{store.count} waiting</b>
    </p>
    <ul class="by">
      {#each perWorkspace as w (w.id)}
        <li><span class="name">{w.name}</span> <b>{w.n}</b></li>
      {/each}
    </ul>
    {#if newest && !inInbox}
      <p class="ask">
        <span class="who">{workspaces.labelOf(newest.request.workspace)}</span>
        <HostName
          host={newest.request.host}
          port={newest.request.port}
          registrable={newest.domain}
        />
      </p>
      <AllowDenyPair
        denyOnly={blocked}
        allowAria="Allow {newest.request.host} for {workspaces.spokenLabelOf(
          newest.request.workspace,
        )}"
        denyAria="Deny {newest.request.host} for {workspaces.spokenLabelOf(
          newest.request.workspace,
        )}"
        onAllow={() => answer("allow")}
        onDeny={() => answer("deny")}
      />
      <a class="review" href="/inbox">Review</a>
    {/if}
  </div>
{:else if store.status === "ready"}
  <p class="strip quiet" aria-live="polite" data-testid="waiting-strip">
    Nothing is waiting for you.
  </p>
{/if}

<DecisionFlow bind:this={flow} {store} heading={() => undefined} />

<style>
  .strip {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2) var(--space-4);
    margin: 0;
    padding: var(--space-2) var(--space-8);
    border-bottom: 1px solid var(--color-border-subtle);
    font-size: var(--text-sm);
  }
  .quiet {
    color: var(--color-text-muted);
  }
  .waiting {
    background: color-mix(
      in srgb,
      var(--color-warning) 14%,
      var(--color-surface)
    );
    border-bottom-color: var(--color-warning);
    color: var(--color-text);
  }
  p {
    margin: 0;
  }
  .tally {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }
  .dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
    background: var(--color-warning);
  }
  .by {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-3);
    margin: 0;
    padding: 0;
    list-style: none;
    color: var(--color-text-muted);
  }
  .by b {
    color: var(--color-text);
  }
  /* The host never shrinks below its own width: it wraps to a line of its own instead of being cut
     inside a word. */
  .ask {
    flex: 1 0 auto;
    max-width: 100%;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    align-items: baseline;
  }
  .who {
    color: var(--color-text-muted);
  }
  .review {
    display: inline-flex;
    align-items: center;
    min-width: 24px;
    min-height: 24px;
    padding-inline: var(--space-2);
    color: var(--color-text);
    font-weight: 600;
  }
  @media (max-width: 760px) {
    .strip {
      padding-inline: var(--space-4);
    }
  }
</style>
