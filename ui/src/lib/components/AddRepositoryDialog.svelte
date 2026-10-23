<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { covering, type Identity } from "#lib/identities/model.ts";
  import { parseRepoUrl } from "#lib/identities/repo.ts";
  import {
    matchingUrl,
    tableCannotHold,
    TABLE_CANNOT_HOLD,
  } from "#lib/repos/model.ts";
  import {
    identities as identityStore,
    type IdentitiesStore,
  } from "#lib/stores/identities.svelte.ts";
  import type { AddRepository } from "#lib/stores/checkouts.svelte.ts";
  import { repoUrlProblem } from "#lib/workspaces/model.ts";
  import {
    directoryFor,
    directoryProblem,
    sameRepositoryKey,
  } from "#lib/workspaces/repo-rows.ts";
  import FormDialog from "./FormDialog.svelte";
  import RepoPicker from "./RepoPicker.svelte";

  // "Add repository": one row of the create form, for a running workspace. The checks are the
  // create form's, plus the two a running workspace adds: the directory may not exist on the disk
  // yet, and the repository may not already be on the list. `onSubmit` returns the service's
  // refusal as text, or `null` when the clone started (the dialog then closes).
  let {
    open = $bindable(false),
    entries,
    onDisk,
    identities = identityStore,
    onSubmit,
  }: {
    open?: boolean;
    /** The list's entries: their directories and addresses are taken. */
    entries: readonly { url: string; directory: string }[];
    /** The directories of the checkouts the listing found: taken too. */
    onDisk: readonly string[];
    identities?: Pick<IdentitiesStore, "identities" | "ensureLoaded">;
    onSubmit: (body: AddRepository) => Promise<string | null>;
  } = $props();

  const id = $props.id();
  let url = $state("");
  let directory = $state("");
  let identity = $state<string | null>(null);
  let listedBy = $state<number | null>(null);
  let pull = $state(true);
  let push = $state(true);
  let touched = $state(false);
  let refusal = $state<string | null>(null);
  let busy = $state(false);

  let wasOpen = false;
  $effect(() => {
    if (open && !wasOpen) {
      url = "";
      directory = "";
      identity = null;
      listedBy = null;
      pull = true;
      push = true;
      touched = false;
      refusal = null;
      void identities.ensureLoaded();
    }
    wasOpen = open;
  });

  const offered = $derived<Identity[]>(
    matchingUrl(identities.identities, url, listedBy),
  );
  const chosenForYou = $derived.by(() => {
    const parsed = parseRepoUrl(url);
    if (!parsed.ok || offered.length === 0) return null;
    const listed = offered.find((i) => i.id === listedBy);
    return (listed ?? covering(offered, parsed.host, parsed.owner))?.id ?? null;
  });
  const selected = $derived(identity ?? String(chosenForYou ?? ""));

  const target = $derived(directoryFor({ url, directory }));
  const urlProblem = $derived.by(() => {
    const bad = repoUrlProblem(url);
    if (bad) return bad;
    const key = sameRepositoryKey(url);
    const same = entries.find((e) => sameRepositoryKey(e.url) === key);
    return same
      ? `This repository is already on the list, as ${same.directory}.`
      : null;
  });
  const directoryError = $derived.by(() => {
    const typed = directory.trim();
    const bad = typed === "" ? null : directoryProblem(typed);
    if (bad) return bad;
    if (entries.some((e) => e.directory === target))
      return `${target} is already on the list. Use another directory.`;
    if (onDisk.includes(target))
      return `${target} already exists in the workspace. Puddle never clones into a folder that holds anything: use another directory, or use "Manage this" on the checkout that is there.`;
    return null;
  });
  const shownUrl = $derived(touched ? urlProblem : null);
  const shownDirectory = $derived(touched ? directoryError : null);

  function picked(repo: { url: string }, by: number | null) {
    url = repo.url;
    listedBy = by;
    identity = null;
    document.getElementById(`${id}-directory`)?.focus();
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    touched = true;
    if (urlProblem || directoryError) {
      document
        .getElementById(urlProblem ? `${id}-url` : `${id}-directory`)
        ?.focus();
      return;
    }
    busy = true;
    try {
      const typed = directory.trim();
      refusal = await onSubmit({
        kind: "clone",
        url: url.trim(),
        ...(typed === "" ? {} : { directory: typed }),
        ...(selected === "" ? {} : { identity_id: Number(selected) }),
        pull,
        push,
      });
      if (refusal === null) open = false;
    } finally {
      busy = false;
    }
  }
</script>

<FormDialog
  bind:open
  title="Add repository"
  description="Puddle clones it into the running workspace, next to what is already there."
>
  <form onsubmit={submit} novalidate>
    <div class="field">
      <label for="{id}-url">Git repository (HTTPS)</label>
      <input
        id="{id}-url"
        type="text"
        inputmode="url"
        autocomplete="off"
        spellcheck="false"
        placeholder="https://github.com/you/project.git"
        bind:value={url}
        oninput={() => {
          listedBy = null;
          identity = null;
          refusal = null;
        }}
        aria-invalid={shownUrl ? "true" : undefined}
        aria-describedby={shownUrl ? `${id}-url-error` : `${id}-url-hint`}
      />
      {#if shownUrl}
        <p class="error" id="{id}-url-error" role="alert">{shownUrl}</p>
      {:else}
        <p class="hint" id="{id}-url-hint">
          SSH addresses aren't supported yet: use the HTTPS URL.
        </p>
      {/if}
    </div>

    <RepoPicker
      identities={identities.identities}
      onPick={(repo, by) => picked(repo, by)}
    />

    {#if tableCannotHold(url)}
      <p class="hint warn" data-testid="table-cannot-hold">
        {TABLE_CANNOT_HOLD}
      </p>
    {/if}

    <div class="field">
      <label for="{id}-directory">Directory</label>
      <input
        id="{id}-directory"
        type="text"
        autocomplete="off"
        spellcheck="false"
        placeholder={directoryFor({ url, directory: "" })}
        bind:value={directory}
        oninput={() => (refusal = null)}
        aria-invalid={shownDirectory ? "true" : undefined}
        aria-describedby={shownDirectory
          ? `${id}-directory-error`
          : `${id}-directory-hint`}
      />
      {#if shownDirectory}
        <p class="error" id="{id}-directory-error" role="alert">
          {shownDirectory}
        </p>
      {:else}
        <p class="hint" id="{id}-directory-hint">
          The folder it is cloned into, on the workspace's disk. It follows the
          address until you type one.
        </p>
      {/if}
    </div>

    <div class="field">
      <label for="{id}-identity">Git identity</label>
      <select
        id="{id}-identity"
        value={selected}
        onchange={(event) => {
          identity = event.currentTarget.value;
        }}
      >
        <option value="">Let Puddle choose</option>
        {#if offered.length > 0}
          <optgroup label="Cover this repository">
            {#each offered as option (option.id)}
              <option value={String(option.id)}
                >{option.label}{option.id === listedBy
                  ? " (listed this repository)"
                  : ""}</option
              >
            {/each}
          </optgroup>
        {/if}
        {#if identities.identities.length > offered.length}
          <optgroup label="Other identities">
            {#each identities.identities.filter((i) => !offered.some((o) => o.id === i.id)) as option (option.id)}
              <option value={String(option.id)}>{option.label}</option>
            {/each}
          </optgroup>
        {/if}
      </select>
    </div>

    <div class="boxes">
      <label class="choice">
        <input
          type="checkbox"
          bind:checked={pull}
          aria-describedby="{id}-boxes-hint"
        />
        Pull
      </label>
      <label class="choice">
        <input
          type="checkbox"
          bind:checked={push}
          aria-describedby="{id}-boxes-hint"
        />
        Push
      </label>
    </div>
    <p class="hint" id="{id}-boxes-hint">
      Pull and Push say what the workspace may fetch from and send to this
      repository. You can change them later on its Git tab.
    </p>

    {#if refusal}<p class="error" role="alert">{refusal}</p>{/if}
    <div class="actions">
      <button type="button" class="btn" onclick={() => (open = false)}
        >Cancel</button
      >
      <button type="submit" class="btn primary" disabled={busy}>Clone</button>
    </div>
  </form>
</FormDialog>

<style>
  .boxes {
    display: flex;
    gap: var(--space-4);
  }
  .boxes .choice {
    min-height: var(--control-size);
  }
  .warn {
    color: var(--color-warning);
  }
</style>
