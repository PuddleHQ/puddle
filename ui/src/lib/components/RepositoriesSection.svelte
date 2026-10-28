<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import AddRepositoryDialog from "./AddRepositoryDialog.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import ManageCheckoutDialog from "./ManageCheckoutDialog.svelte";
  import { absoluteTime, relativeTime } from "#lib/format/relative-time.ts";
  import {
    CheckoutsStore,
    type AddRepository,
  } from "#lib/stores/checkouts.svelte.ts";
  import { identities } from "#lib/stores/identities.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import { WorkspaceGitStore } from "#lib/stores/workspace-git.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import {
    addProblem,
    branchLabel,
    cloneAgainProblem,
    missingEntries,
    remoteDiffers,
    servingIdentity,
    stoppedWords,
    unreadableLabel,
    type ListEntry,
  } from "#lib/workspaces/checkouts.ts";
  import { operationLabel, type Workspace } from "#lib/workspaces/model.ts";
  import { tabHref } from "#lib/workspaces/tabs.ts";
  import "#lib/theme/controls.css";

  // The workspace page's Repositories section: what is on the disk next to the workspace's list.
  // Everything the guest reported (names, branches, addresses, errors) is shown as text only.
  let { workspace }: { workspace: Workspace } = $props();

  const store = new CheckoutsStore();
  const git = new WorkspaceGitStore();
  const name = $derived(workspace.name);

  onMount(() => identities.start());
  $effect(() => {
    if (name) return store.start(name);
  });
  $effect(() => {
    if (name) return git.start(name);
  });

  const listing = $derived(store.listing);
  const running = $derived(workspace.status === "running");
  const entries = $derived(workspace.repositories);
  const missing = $derived(missingEntries(entries, listing));
  const addOff = $derived(addProblem(workspace));
  const cloneOff = $derived(cloneAgainProblem(workspace));
  const progress = $derived(workspaces.progress[name]);
  const onDisk = $derived(listing?.checkouts.map((c) => c.directory) ?? []);
  // A listing with nothing on it and no entry has nothing to show; a stopped workspace with no
  // listing still shows its list entries, unchecked.
  const unchecked = $derived(
    listing === null ||
      (listing.checkouts.length === 0 &&
        (listing.problem !== null || listing.checked_at === null)),
  );

  let heading = $state<HTMLElement>();
  let addOpen = $state(false);
  let manageOpen = $state(false);
  let managing = $state<{ directory: string; url: string } | null>(null);
  let removeOpen = $state(false);
  let removing = $state("");
  let problem = $state<string | null>(null);
  let focusAfterManage = false;

  const identityFor = (url: string) =>
    git.git ? servingIdentity(git.git.identities, url) : null;

  /** The identity Puddle picks for a checkout when none is chosen (the host's rule). */
  function pickedFor(url: string) {
    return (
      identityFor(url) ??
      servingIdentity(identities.identities, url) ??
      identities.identities.find((i) => i.is_default) ??
      null
    );
  }

  async function focusHeading() {
    await tick();
    heading?.focus();
  }

  async function add(body: AddRepository): Promise<string | null> {
    const result = await store.add(body);
    if (!result.ok) return result.message;
    toasts.push(
      `Cloning ${body.kind === "clone" ? (body.directory ?? "the repository") : body.directory}.`,
    );
    return null;
  }

  async function manage(
    checkout: { directory: string; url: string },
    choice: { identity: number | null; pull: boolean; push: boolean },
  ): Promise<string | null> {
    const result = await store.add({
      kind: "existing",
      directory: checkout.directory,
      url: checkout.url,
      ...(choice.identity === null ? {} : { identity_id: choice.identity }),
      pull: choice.pull,
      push: choice.push,
    });
    if (!result.ok) return result.message;
    toasts.push(`${checkout.directory} is on the list now.`);
    focusAfterManage = true;
    return null;
  }

  async function cloneAgain(entry: ListEntry) {
    problem = null;
    const result = await store.cloneAgain(entry.directory);
    if (!result.ok) problem = result.message;
    else toasts.push(`Cloning ${entry.directory} again.`);
  }

  async function remove() {
    const directory = removing;
    problem = null;
    const result = await store.remove(directory);
    if (!result.ok) {
      problem = result.message;
      return;
    }
    toasts.push(
      `Removed ${directory} from the list. Its files stay in the workspace.`,
    );
    await focusHeading();
  }

  function askRemove(entry: ListEntry) {
    removing = entry.directory;
    removeOpen = true;
  }

  function askManage(directory: string, url: string) {
    managing = { directory, url };
    manageOpen = true;
  }
</script>

{#snippet identityLine(entry: ListEntry)}
  {@const identity = identityFor(entry.url)}
  {#if git.git}
    <span class="meta">
      {#if identity}
        Identity: <a href="/identities/{identity.id}">{identity.label}</a>
      {:else}
        No identity covers this: <a href={tabHref(workspace.id, "git")}
          >open the Git tab</a
        >
      {/if}
    </span>
  {/if}
{/snippet}

{#snippet removeButton(entry: ListEntry)}
  <button
    type="button"
    class="btn"
    aria-label="Remove from the list: {entry.directory}"
    onclick={() => askRemove(entry)}>Remove from the list</button
  >
{/snippet}

<section class="card" id="repositories" aria-labelledby="repos-h">
  <div class="head">
    <h2 id="repos-h" tabindex="-1" bind:this={heading}>Repositories</h2>
    <button
      type="button"
      class="btn"
      disabled={addOff !== null}
      aria-describedby={addOff ? "repos-add-reason" : undefined}
      onclick={() => (addOpen = true)}>Add repository</button
    >
  </div>
  {#if addOff}
    <p class="hint" id="repos-add-reason">{addOff}</p>
  {/if}
  {#if workspace.busy === "cloning"}
    <p class="hint" role="status">
      {operationLabel(workspace.busy)}{progress?.detail
        ? `: ${progress.detail}`
        : ""}. Stop and delete wait until it ends.
    </p>
  {/if}
  {#if problem}<p class="error" role="alert">{problem}</p>{/if}

  {#if store.status === "loading" && listing === null}
    <p class="hint">Reading the checkouts&hellip;</p>
  {:else if store.status === "failed"}
    <p class="error" role="alert">
      Puddle could not list the checkouts. {store.error}
    </p>
  {:else if listing}
    {#if !running}
      <p class="hint">
        {stoppedWords(listing)}
        {#if listing.checked_at !== null}
          <time
            datetime={new Date(listing.checked_at).toISOString()}
            title={absoluteTime(listing.checked_at)}
            >Listed {relativeTime(listing.checked_at, Date.now())}.</time
          >
        {/if}
      </p>
    {:else if listing.problem}
      <p class="error" role="alert">
        Puddle could not list the checkouts: {listing.problem}
      </p>
    {/if}
  {/if}

  {#if listing && !unchecked}
    {#if listing.checkouts.length === 0 && missing.length === 0}
      <p class="hint">No checkout is on the disk and none is listed.</p>
    {/if}
    <ul class="rows" aria-label="Checkouts of {workspace.display_name}">
      {#each listing.checkouts as checkout (checkout.directory)}
        {@const entry = checkout.entry}
        {@const remoteUrl = checkout.remote_url}
        <li>
          <div class="what">
            <b class="mono">{checkout.directory}</b>
            {#if entry}
              <span class="chip">On the list</span>
            {:else}
              <span class="chip">Not managed by Puddle: works as is</span>
            {/if}
            {#if checkout.unreadable === null}
              <span class="meta">{branchLabel(checkout)}</span>
              {#if checkout.dirty}<span class="chip warn">changes</span>{/if}
              {#if checkout.remote_url}
                <span class="meta mono">{checkout.remote_url}</span>
              {/if}
            {:else}
              <span class="meta">{unreadableLabel(checkout)}</span>
            {/if}
            {#if entry}
              {#if remoteDiffers(checkout)}
                <span class="meta"
                  >The list says <span class="mono">{entry.url}</span></span
                >
              {/if}
              {@render identityLine(entry)}
            {/if}
          </div>
          <div class="acts">
            {#if entry}
              {@render removeButton(entry)}
            {:else if checkout.manage.offered && remoteUrl}
              <button
                type="button"
                class="btn"
                aria-label="Manage this: {checkout.directory}"
                onclick={() => askManage(checkout.directory, remoteUrl)}
                >Manage this</button
              >
            {:else if checkout.manage.reason}
              <span class="meta">{checkout.manage.reason}</span>
            {/if}
          </div>
        </li>
      {/each}
      {#each missing as entry (entry.directory)}
        <li>
          <div class="what">
            <b class="mono">{entry.directory}</b>
            <span class="chip warn">Not on the disk</span>
            <span class="meta mono">{entry.url}</span>
            {@render identityLine(entry)}
          </div>
          <div class="acts">
            <button
              type="button"
              class="btn"
              disabled={cloneOff !== null}
              aria-label="Clone again: {entry.directory}"
              aria-describedby={cloneOff ? "repos-clone-reason" : undefined}
              onclick={() => void cloneAgain(entry)}>Clone again</button
            >
            {@render removeButton(entry)}
          </div>
        </li>
      {/each}
    </ul>
    {#if cloneOff && missing.length > 0}
      <p class="hint" id="repos-clone-reason">{cloneOff}</p>
    {/if}
    {#if listing.more > 0}
      <p class="hint">
        And {listing.more} more {listing.more === 1 ? "checkout" : "checkouts"}
        that {listing.more === 1 ? "is" : "are"} not shown here.
      </p>
    {/if}
  {:else if store.status !== "loading" && entries.length > 0}
    <ul
      class="rows"
      aria-label="Repositories listed for {workspace.display_name}"
    >
      {#each entries as entry (entry.directory)}
        <li>
          <div class="what">
            <b class="mono">{entry.directory}</b>
            <span class="chip">On the list</span>
            <span class="meta mono">{entry.url}</span>
            {@render identityLine(entry)}
          </div>
          <div class="acts">{@render removeButton(entry)}</div>
        </li>
      {/each}
    </ul>
  {:else if store.status !== "loading" && listing}
    <p class="hint">No repository is listed for this workspace.</p>
  {/if}
</section>

<AddRepositoryDialog bind:open={addOpen} {entries} {onDisk} onSubmit={add} />

{#if managing}
  {@const checkout = managing}
  <ManageCheckoutDialog
    bind:open={manageOpen}
    directory={managing.directory}
    url={managing.url}
    identities={identities.identities}
    chosen={pickedFor(managing.url)}
    onSubmit={(choice) => manage(checkout, choice)}
    returnFocusTo={() => {
      const goes = focusAfterManage;
      focusAfterManage = false;
      return goes ? heading : undefined;
    }}
  />
{/if}

<ConfirmDialog
  bind:open={removeOpen}
  title="Remove {removing} from the list?"
  summary="Puddle stops managing it: the folder and its files stay in the workspace."
  detail="The checkout keeps working with plain Git. Its Git settings and identities stay as they are, and you can manage it again later."
  confirmLabel="Remove from the list"
  onConfirm={() => void remove()}
/>

<style>
  .card {
    display: grid;
    gap: var(--space-3);
    grid-column: 1 / -1;
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-3);
    justify-content: space-between;
  }
  h2 {
    font-size: var(--text-md);
  }
  p {
    margin: 0;
  }
  .hint {
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .error {
    color: var(--color-danger);
    font-size: var(--text-sm);
  }
  .rows {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .rows li {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-3);
    justify-content: space-between;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .what {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-3);
    min-width: 0;
    flex: 1;
    overflow-wrap: anywhere;
  }
  .meta {
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
  }
  .chip.warn {
    border-color: var(--color-warning);
  }
  .btn:disabled {
    cursor: default;
    opacity: 0.6;
  }
</style>
