<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { workspaceNameError } from "#lib/rules/model.ts";
  import {
    entryPattern,
    globalState,
    isOn,
    type RuleSet,
    type EntryPage,
    type RuleSetEntry,
  } from "#lib/rules/sets.ts";
  import { workspaces as workspaceStore } from "#lib/stores/workspaces.svelte.ts";
  import SetChip from "./SetChip.svelte";
  import WorkspaceSelect from "./WorkspaceSelect.svelte";
  import "#lib/theme/controls.css";

  // One rule set: what it is, its switch for every workspace and the workspaces that switch it
  // for themselves, and its entries. A set you made also has its own actions.
  let {
    set,
    workspaces,
    onSwitch,
    onAddEntry,
    onRename,
    onDelete,
    onDeleteEntry,
    loadEntries,
  }: {
    set: RuleSet;
    /** Workspace names to suggest for a per-workspace switch. */
    workspaces: string[];
    onSwitch: (
      set: RuleSet,
      workspace: string | null,
      enabled: boolean | null,
    ) => void;
    onAddEntry: (set: RuleSet) => void;
    onRename: (set: RuleSet) => void;
    onDelete: (set: RuleSet) => void;
    onDeleteEntry: (set: RuleSet, entry: RuleSetEntry) => void;
    /** Reads a page of a big built-in set's entries, optionally searched. */
    loadEntries: (
      set: RuleSet,
      query: string,
      offset: number,
    ) => Promise<EntryPage>;
  } = $props();

  const id = $props.id();
  const everywhere = $derived(isOn(set, null));
  const mine = $derived(set.kind === "user");
  // What switching the set on does, in words: a set is all allows, all denies or a mix.
  const effects = $derived(new Set(set.entries.map((entry) => entry.effect)));
  const does = $derived(
    effects.has("deny")
      ? effects.has("allow")
        ? "Allows and blocks"
        : "Blocks"
      : effects.has("allow")
        ? "Allows"
        : null,
  );
  let workspace = $state("");
  let problem = $state<string | null>(null);

  // A big built-in set lists only its first entries; the rest is searched and paged in.
  const paged = $derived(set.entry_count > set.entries.length);
  let query = $state("");
  let listed = $state.raw<RuleSetEntry[] | null>(null);
  let matches = $state<number | null>(null);
  let loadProblem = $state<string | null>(null);
  const shown = $derived(listed ?? set.entries);
  const total = $derived(matches ?? set.entry_count);

  async function load(more: boolean) {
    loadProblem = null;
    const text = query.trim();
    if (!more && text === "") {
      listed = null;
      matches = null;
      return;
    }
    try {
      const page = await loadEntries(set, text, more ? shown.length : 0);
      listed = more ? [...shown, ...page.entries] : page.entries;
      matches = page.total;
    } catch {
      loadProblem = "Puddle couldn't read the entries.";
    }
  }

  function switchHere(enabled: boolean) {
    problem = workspaceNameError(workspace.trim());
    if (problem) return;
    onSwitch(set, workspace.trim(), enabled);
    workspace = "";
  }

  function changed(at: number): string {
    return new Intl.DateTimeFormat(undefined, { dateStyle: "medium" }).format(
      at,
    );
  }
</script>

<article class="set" aria-labelledby="{id}-name" data-set-id={set.id}>
  <header>
    <h3 id="{id}-name">
      <SetChip name={set.name} fill={set.fill} variant="chip" />
    </h3>
    <span class="chip">{mine ? "Yours" : "Built in"}</span>
    {#if does !== null}
      <span class="chip {does === 'Allows' ? 'allow' : 'deny'}">{does}</span>
    {/if}
    <span class="muted mono" title="The id refusals and the activity log use"
      >{set.id}</span
    >
    {#if set.changed_at !== null}
      <span class="chip warn">Updated by Puddle {changed(set.changed_at)}</span>
    {/if}
  </header>
  {#if set.description !== ""}<p class="desc">{set.description}</p>{/if}

  <div class="row">
    <label class="switch">
      <input
        type="checkbox"
        role="switch"
        checked={everywhere}
        onchange={(e) => {
          const next = e.currentTarget.checked;
          e.currentTarget.checked = everywhere;
          onSwitch(set, null, next);
        }}
      />
      On for every workspace
    </label>
    <span class="muted">{globalState(set)}</span>
    {#if set.global !== null}
      <button
        type="button"
        class="btn"
        onclick={() => onSwitch(set, null, null)}
        >Back to the default ({set.default_on ? "on" : "off"})</button
      >
    {/if}
  </div>

  {#if set.overrides.length > 0}
    <ul class="overrides" aria-label="Workspaces that switch {set.name}">
      {#each set.overrides as o (o.workspace)}
        <li>
          <span
            ><b>{o.enabled ? "On" : "Off"}</b> in
            <b>{workspaceStore.labelOf(o.workspace)}</b></span
          >
          <button
            type="button"
            class="btn"
            onclick={() => onSwitch(set, o.workspace, null)}
            >Follow every workspace</button
          >
        </li>
      {/each}
    </ul>
  {/if}

  <form
    class="row"
    aria-label="Switch {set.name} for one workspace"
    onsubmit={(e) => e.preventDefault()}
    novalidate
  >
    <div class="field">
      <label for="{id}-ws">Workspace</label>
      <WorkspaceSelect
        id="{id}-ws"
        bind:value={workspace}
        known={workspaces}
        invalid={problem !== null}
        describedby={problem ? `${id}-ws-error` : undefined}
      />
    </div>
    <button type="button" class="btn" onclick={() => switchHere(true)}
      >On there</button
    >
    <button type="button" class="btn" onclick={() => switchHere(false)}
      >Off there</button
    >
    {#if problem}
      <p class="error" id="{id}-ws-error" role="alert">{problem}</p>
    {/if}
  </form>

  <details>
    <summary
      >{set.entry_count}
      {set.entry_count === 1 ? "entry" : "entries"}</summary
    >
    {#if paged}
      <form
        class="row"
        role="search"
        aria-label="Search the entries of {set.name}"
        onsubmit={(e) => {
          e.preventDefault();
          void load(false);
        }}
      >
        <div class="field">
          <label for="{id}-q">Search entries</label>
          <input
            id="{id}-q"
            type="search"
            autocomplete="off"
            spellcheck="false"
            bind:value={query}
          />
        </div>
        <button type="submit" class="btn">Search</button>
      </form>
    {/if}
    {#if shown.length === 0}
      <p class="muted">
        {matches !== null
          ? "No entry matches."
          : "No entries yet. Add one, or approve a request into this set from the Inbox."}
      </p>
    {:else}
      <ul class="entries">
        {#each shown as entry (entry.rule_id ?? entry.pattern)}
          <li>
            <span class="chip {entry.effect}"
              >{entry.effect === "allow" ? "Allow" : "Deny"}</span
            >
            <span class="mono">{entryPattern(entry)}</span>
            {#if entry.note !== ""}<span class="muted">{entry.note}</span>{/if}
            {#if mine}
              <button
                type="button"
                class="btn"
                aria-label="Delete {entryPattern(entry)} from {set.name}"
                onclick={() => onDeleteEntry(set, entry)}>Delete</button
              >
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
    {#if paged}
      <p class="muted" aria-live="polite">
        Showing {shown.length} of {total}{matches !== null ? " matches" : ""}.
      </p>
      {#if shown.length < total}
        <button type="button" class="btn" onclick={() => void load(true)}
          >Show more</button
        >
      {/if}
      {#if loadProblem}<p class="error" role="alert">{loadProblem}</p>{/if}
    {/if}
  </details>

  {#if mine}
    <div class="row">
      <button type="button" class="btn" onclick={() => onAddEntry(set)}
        >Add entry</button
      >
      <button type="button" class="btn" onclick={() => onRename(set)}
        >Edit</button
      >
      <button type="button" class="btn deny" onclick={() => onDelete(set)}
        >Delete set</button
      >
    </div>
  {/if}
</article>

<style>
  .set {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  h3 {
    margin: 0;
    font-size: var(--text-md);
  }
  .desc,
  .muted {
    margin: 0;
    color: var(--color-text-muted);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: var(--space-2);
  }
  .switch {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    min-height: 1.75rem;
    font-weight: 600;
  }
  .field {
    display: grid;
    gap: var(--space-1);
  }
  .field label {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }

  .overrides,
  .entries {
    display: grid;
    gap: var(--space-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .overrides li,
  .entries li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    min-height: 1.75rem;
  }
  .mono {
    font-family: var(--font-mono);
    overflow-wrap: anywhere;
  }
  .error {
    margin: 0;
    color: var(--color-danger);
  }
  summary {
    cursor: pointer;
    min-height: 1.5rem;
  }
  .chip {
    display: inline-block;
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .chip.allow {
    color: var(--color-success);
    border-color: var(--color-success);
  }
  .chip.deny {
    color: var(--color-danger);
    border-color: var(--color-danger);
  }
  .chip.warn {
    color: var(--color-warning);
    border-color: var(--color-warning);
  }
</style>
