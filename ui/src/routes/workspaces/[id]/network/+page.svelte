<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import LoadNotice from "#lib/components/LoadNotice.svelte";
  import RequestRulesSection from "#lib/components/RequestRulesSection.svelte";
  import RuleTable from "#lib/components/RuleTable.svelte";
  import WorkspaceSets from "#lib/components/WorkspaceSets.svelte";
  import { DEFAULT_SORT, isOwn, view, workspaceOf } from "#lib/rules/model.ts";
  import { pending } from "#lib/stores/pending.svelte.ts";
  import { ruleSets } from "#lib/stores/rule-sets.svelte.ts";
  import { rulesStore } from "#lib/stores/rules.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import { tabHref } from "#lib/workspaces/tabs.ts";
  import { NO_FILTER } from "#lib/rules/model.ts";
  import "#lib/theme/controls.css";

  const id = $derived(page.params["id"] ?? "");
  const workspace = $derived(workspaces.list.find((w) => w.id === id));
  let now = $state(Date.now());

  onMount(() => {
    const stopRules = rulesStore.start();
    const stopSets = ruleSets.start();
    const clock = setInterval(() => {
      now = Date.now();
    }, 30_000);
    return () => {
      stopRules();
      stopSets();
      clearInterval(clock);
    };
  });

  const rows = $derived(
    pending.rows.filter((r) => r.request.workspace === workspace?.id),
  );
  const rules = $derived(
    view(
      rulesStore.rules.filter((r) => {
        if (!isOwn(r)) return false;
        const owner = workspaceOf(r);
        return owner === null || owner === workspace?.id;
      }),
      NO_FILTER,
      DEFAULT_SORT,
      now,
    ),
  );
</script>

{#if workspace}
  {#if rows.length > 0}
    <p class="waiting-note" role="status">
      {rows.length}
      {rows.length === 1 ? "request is" : "requests are"} waiting for {workspace.name}.
      <a href={tabHref(workspace.id, "")}>Decide on the Overview</a>
    </p>
  {/if}

  <section class="block" aria-labelledby="rules-h">
    <div class="head">
      <h2 id="rules-h">Rules that apply</h2>
      <a class="btn" href="/rules">All rules</a>
    </div>
    {#if rulesStore.status === "loading"}
      <p class="muted pad">Loading rules&hellip;</p>
    {:else if rulesStore.status === "failed"}
      <LoadNotice what="the rules" message={rulesStore.loadError} padded />
    {:else if rules.length === 0}
      {#if rulesStore.loadError}
        <LoadNotice
          what="the rules"
          message={rulesStore.loadError}
          stale
          padded
        />
      {/if}
      <p class="muted pad">
        No rule applies to {workspace.name} yet. Requests wait here until you decide.
      </p>
    {:else}
      {#if rulesStore.loadError}
        <LoadNotice
          what="the rules"
          message={rulesStore.loadError}
          stale
          padded
        />
      {/if}
      <RuleTable {rules} sort={DEFAULT_SORT} {now} />
      <p class="muted note">
        Change or delete a rule on the <a href="/rules">Rules page</a>.
      </p>
    {/if}
  </section>

  <RequestRulesSection workspace={workspace.id} {now} />

  <WorkspaceSets workspace={workspace.id} />
{/if}

<style>
  .block {
    margin-bottom: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  h2 {
    padding: var(--space-3) var(--space-4);
    font-size: var(--text-md);
    border-bottom: 1px solid var(--color-border-subtle);
  }
  .waiting-note {
    margin: 0 0 var(--space-4);
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
    background: var(--color-surface-raised);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-inline-end: var(--space-4);
  }
  .head h2 {
    flex: 1;
    border-bottom: 0;
  }
  .muted {
    margin: 0;
    color: var(--color-text-muted);
  }
  .pad {
    padding: var(--space-4);
  }
  .note {
    padding: var(--space-2) var(--space-4) var(--space-3);
    font-size: var(--text-sm);
  }
</style>
