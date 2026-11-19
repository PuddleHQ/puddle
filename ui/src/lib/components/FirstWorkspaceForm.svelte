<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import {
    workspaces as defaultStore,
    type WorkspaceStore,
  } from "#lib/stores/workspaces.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import { allowedNotice } from "#lib/workspaces/hosts.ts";
  import {
    isIncompleteAddress,
    nameProblem,
    resolveRepoAddress,
    suggestName,
  } from "#lib/workspaces/model.ts";
  import "#lib/theme/controls.css";

  // What the Workspaces screen shows when there is no workspace: the form itself, not a box that
  // tells you to press a button. The address is the one thing the common case needs; the name
  // follows it, and the full form is one button away for anything more.
  let {
    store = defaultStore,
    onMore,
  }: {
    store?: Pick<WorkspaceStore, "create"> & { list: WorkspaceStore["list"] };
    /** Opens the full form, for several repositories, a folder, resources or a name of your own. */
    onMore: () => void;
  } = $props();

  let address = $state("");
  let left = $state(false);
  let refusal = $state<string | null>(null);
  let working = $state(false);
  const id = $props.id();

  const resolved = $derived(resolveRepoAddress(address));
  const shown = $derived.by(() => {
    if (refusal) return refusal;
    if (address.trim() === "" || !("message" in resolved)) return null;
    return isIncompleteAddress(resolved.message) && !left
      ? null
      : resolved.message;
  });
  const will = $derived(
    "url" in resolved && resolved.url !== address.trim()
      ? `Will clone ${resolved.url}`
      : null,
  );
  const name = $derived("url" in resolved ? suggestName(resolved.url) : "");

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (working) return;
    if (!("url" in resolved)) {
      refusal = resolved.message;
      return;
    }
    const clash = nameProblem(name, null, store.list);
    if (clash) {
      refusal = clash;
      return;
    }
    working = true;
    const result = await store.create({
      name,
      repositories: [{ url: resolved.url }],
    });
    working = false;
    if (!result.ok) {
      refusal = result.message;
      return;
    }
    address = "";
    const allowed = allowedNotice(result.value.allowed_hosts);
    if (allowed) toasts.push(allowed, { ms: 10_000 });
  }
</script>

<form class="first" onsubmit={submit} novalidate>
  <h2>Create your first workspace</h2>
  <p class="lead">
    A workspace is an isolated copy of a repository where you and your tools
    work.
  </p>
  <div class="form-field">
    <label for="{id}-address">Repository address</label>
    <input
      id="{id}-address"
      class="input-fill"
      type="text"
      autocomplete="off"
      spellcheck="false"
      placeholder="https://github.com/owner/project"
      bind:value={address}
      oninput={() => {
        refusal = null;
      }}
      onblur={() => {
        left = true;
      }}
      aria-invalid={shown ? "true" : undefined}
      aria-describedby={shown ? `${id}-problem` : undefined}
    />
    {#if shown}
      <p class="problem" id="{id}-problem" role="alert">{shown}</p>
    {:else if will}
      <p class="hint">{will}</p>
    {/if}
  </div>
  {#if name}
    <p class="hint">It will be called {name}.</p>
  {/if}
  <div class="acts">
    <button type="submit" class="btn primary" disabled={working}
      >Create workspace</button
    >
    <button type="button" class="btn" onclick={onMore}>More options</button>
  </div>
</form>

<style>
  .first {
    display: grid;
    gap: var(--space-3);
    max-width: 36rem;
  }
  h2,
  p {
    margin: 0;
  }
  .lead,
  .hint {
    color: var(--color-text-muted);
  }
  .problem {
    color: var(--color-danger);
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
</style>
