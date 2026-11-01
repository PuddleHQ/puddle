<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import {
    coverageOf,
    coverageProblem,
    describeSource,
    foundLabel,
    hasAuthor,
    hasOwner,
    hostProblem,
    needsOrg,
    orgProblem,
    parseOwners,
    sourceOf,
    toggleOwner,
    tokenProblem,
    type AccountProfile,
    type CredentialDraft,
    type FoundAccount,
    type Source,
  } from "#lib/identities/model.ts";
  import type { IdentitiesStore } from "#lib/stores/identities.svelte.ts";
  import "#lib/theme/controls.css";

  // Adds one credential to an identity: an account already signed in on this computer, or a token
  // the user pastes (kept in the operating system's credential store and never shown again), plus
  // what the credential covers on its host. Nothing is saved until the identity is.
  let {
    store,
    existing,
    onAdd,
    onCancel,
    onProfile,
  }: {
    store: IdentitiesStore;
    existing: readonly CredentialDraft[];
    /** `stored` is the source of a token that was just kept, so the dialog can remove it again if the identity is not saved. */
    onAdd: (credential: CredentialDraft, stored: Source | null) => void;
    onCancel: () => void;
    /** The account's name and address when the Git host gave them, for the author fields. */
    onProfile: (profile: AccountProfile) => void;
  } = $props();

  const id = $props.id();
  let mode = $state<"found" | "paste">("found");
  let pick = $state<number | null>(null);
  let host = $state("github.com");
  let org = $state("");
  let token = $state("");
  let ownersText = $state("");
  let rest = $state(true);
  let problem = $state<{ field: string; message: string } | null>(null);
  let busy = $state(false);
  let root = $state<HTMLElement>();
  /** What the Git host said of the chosen or looked-up account; looking is never required to save. */
  let lookup = $state<
    | { state: "idle" }
    | { state: "looking" }
    | { state: "failed"; message: string }
    | { state: "ready"; profile: AccountProfile }
  >({ state: "idle" });
  let lookups = 0;
  /** A pasted token kept so the account could be asked about it; reused by Add credential. */
  let kept: { key: string; source: Source; used: boolean } | null = null;

  const usable = $derived(
    (store.found?.accounts ?? []).filter((a) => sourceOf(a) !== null),
  );
  const chosen = $derived<FoundAccount | null>(
    pick === null ? null : (usable[pick] ?? null),
  );
  const coverHost = $derived(
    mode === "found"
      ? (chosen?.host ?? "this host")
      : host.trim() || "this host",
  );

  onDestroy(() => {
    if (kept && !kept.used) void store.forgetToken(kept.source);
  });

  async function look(source: Source) {
    const mine = ++lookups;
    lookup = { state: "looking" };
    const answer = await store.profileOf(source);
    if (mine !== lookups) return;
    if (!answer.ok) {
      lookup = { state: "failed", message: answer.message };
      return;
    }
    lookup = { state: "ready", profile: answer.value };
    if (hasAuthor(answer.value)) onProfile(answer.value);
  }

  function forget() {
    lookups++;
    lookup = { state: "idle" };
  }

  const pastedKey = () =>
    `${host.trim().toLowerCase()}|${needsOrg(host) ? org.trim() : ""}|${token}`;

  /** Keeps the pasted token (the account can only be asked about a kept one) and asks the host. */
  async function lookUpPasted() {
    problem = null;
    const hostError = hostProblem(host);
    if (hostError) return fail("host", hostError);
    if (needsOrg(host)) {
      const orgError = orgProblem(org);
      if (orgError) return fail("org", orgError);
    }
    const tokenError = tokenProblem(token);
    if (tokenError) return fail("token", tokenError);
    const key = pastedKey();
    if (kept?.key !== key) {
      busy = true;
      try {
        const answer = await store.storeToken(
          host.trim().toLowerCase(),
          needsOrg(host) ? org.trim() : null,
          token,
        );
        if (!answer.ok) return fail("token", answer.message);
        if (kept) void store.forgetToken(kept.source);
        kept = { key, source: answer.value, used: false };
      } finally {
        busy = false;
      }
    }
    if (kept) await look(kept.source);
  }

  onMount(() => {
    if (store.found === null) void store.loadFound();
  });

  function choose(account: FoundAccount, index: number) {
    pick = index;
    const covers = coverageOf(account);
    ownersText = covers.owners.join(", ");
    rest = covers.rest_of_host;
    const source = sourceOf(account);
    if (source) void look(source);
  }

  function switchMode(next: "found" | "paste") {
    mode = next;
    problem = null;
    forget();
    if (next === "paste") {
      ownersText = "";
      rest = true;
    } else if (chosen) {
      choose(chosen, pick ?? 0);
    }
  }

  async function fail(field: string, message: string) {
    problem = { field, message };
    await tick();
    root?.querySelector<HTMLElement>(`#${id}-${field}`)?.focus();
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    problem = null;
    let source: Source | null = null;
    let credentialHost: string;
    if (mode === "found") {
      source = chosen ? sourceOf(chosen) : null;
      if (!chosen || !source) {
        return fail("found", "Pick an account, or paste a token instead.");
      }
      credentialHost = chosen.host;
    } else {
      credentialHost = host.trim().toLowerCase();
      const hostError = hostProblem(host);
      if (hostError) return fail("host", hostError);
      if (needsOrg(credentialHost)) {
        const orgError = orgProblem(org);
        if (orgError) return fail("org", orgError);
      }
      const tokenError = tokenProblem(token);
      if (tokenError) return fail("token", tokenError);
    }
    const owners = parseOwners(ownersText);
    if (!owners.ok) return fail("owners", owners.message);
    const covers = { owners: owners.owners, rest_of_host: rest };
    const coverError = coverageProblem(covers);
    if (coverError) return fail("owners", coverError);
    const line = source ? describeSource(source) : null;
    if (line && existing.some((c) => describeSource(c.source) === line)) {
      return fail("found", "That credential is already on this identity.");
    }

    let stored: Source | null = null;
    if (mode === "paste" && kept?.key === pastedKey()) {
      kept.used = true;
      source = kept.source;
      stored = kept.source;
      token = "";
    } else if (mode === "paste") {
      busy = true;
      try {
        const answer = await store.storeToken(
          credentialHost,
          needsOrg(credentialHost) ? org.trim() : null,
          token,
        );
        if (!answer.ok) return fail("token", answer.message);
        source = answer.value;
        stored = answer.value;
        token = "";
      } finally {
        busy = false;
      }
    }
    if (source) onAdd({ host: credentialHost, source, covers }, stored);
  }
</script>

<form
  class="editor"
  onsubmit={submit}
  novalidate
  bind:this={root}
  aria-label="Add a credential"
>
  <fieldset>
    <legend>Where does the credential come from?</legend>
    <label class="choice">
      <input
        type="radio"
        name="{id}-mode"
        checked={mode === "found"}
        onchange={() => switchMode("found")}
      />
      Found on this computer
    </label>
    {#if mode === "found"}
      <div class="found" id="{id}-found" tabindex="-1">
        {#if store.foundStatus === "loading"}
          <p class="hint">Looking for signed-in accounts&hellip;</p>
        {:else if store.foundStatus === "failed"}
          <p class="hint">
            Puddle couldn't look for signed-in accounts. Paste a token instead.
          </p>
        {:else}
          {#if usable.length === 0}
            <p class="hint">
              No signed-in GitHub CLI or Git Credential Manager accounts were
              found.
            </p>
          {/if}
          {#each usable as account, index (index)}
            <label class="choice">
              <input
                type="radio"
                name="{id}-account"
                checked={pick === index}
                onchange={() => choose(account, index)}
              />
              <span>
                {foundLabel(account)}
                {#if !account.signed_in}
                  <span class="chip warn">sign-in expired</span>
                {/if}
              </span>
            </label>
          {/each}
          {#each store.found?.problems ?? [] as p, index (index)}
            <p class="hint">{p.message}.</p>
          {/each}
        {/if}
      </div>
    {/if}
    <label class="choice">
      <input
        type="radio"
        name="{id}-mode"
        checked={mode === "paste"}
        onchange={() => switchMode("paste")}
      />
      Paste a token
    </label>
  </fieldset>

  {#if mode === "paste"}
    <div class="field">
      <label for="{id}-host">Git host</label>
      <input
        id="{id}-host"
        type="text"
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        bind:value={host}
        aria-invalid={problem?.field === "host" ? "true" : undefined}
        aria-describedby={problem?.field === "host" ? `${id}-error` : undefined}
      />
    </div>
    {#if needsOrg(host)}
      <div class="field">
        <label for="{id}-org">Azure DevOps organisation</label>
        <input
          id="{id}-org"
          type="text"
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          bind:value={org}
          aria-invalid={problem?.field === "org" ? "true" : undefined}
          aria-describedby={problem?.field === "org"
            ? `${id}-error`
            : undefined}
        />
        <p class="hint">An Azure DevOps token belongs to one organisation.</p>
      </div>
    {/if}
    <div class="field">
      <label for="{id}-token">Token</label>
      <input
        id="{id}-token"
        type="password"
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        bind:value={token}
        aria-invalid={problem?.field === "token" ? "true" : undefined}
        aria-describedby={problem?.field === "token"
          ? `${id}-error`
          : `${id}-token-hint`}
      />
      <p class="hint" id="{id}-token-hint">
        Puddle keeps it in your operating system's credential store and never
        shows it again.
      </p>
      <div>
        <button
          type="button"
          class="btn"
          disabled={busy || lookup.state === "looking"}
          onclick={lookUpPasted}>Look up this account</button
        >
      </div>
    </div>
  {/if}

  {#if lookup.state !== "idle"}
    <div class="profile" aria-live="polite">
      {#if lookup.state === "looking"}
        <p class="hint">Asking {coverHost} who this is&hellip;</p>
      {:else if lookup.state === "failed"}
        <p class="hint">
          {lookup.message} You can still add the credential and type the author yourself.
        </p>
      {:else}
        {@const found = lookup.profile}
        {#if found.problem}
          <p class="hint">{found.problem.message}</p>
        {:else if hasAuthor(found)}
          <p class="hint">
            {found.account ?? "This account"} on {coverHost}: {found.name ??
              "no name"}{found.email ? ` <${found.email}>` : ""}.
          </p>
        {/if}
        {#if !found.problem && !hasAuthor(found) && needsOrg(coverHost)}
          <p class="hint">
            Azure DevOps doesn't tell Puddle who you are: type the author name
            and address yourself.
          </p>
        {/if}
        {#each found.notes as note, index (index)}
          <p class="hint">{note.message}</p>
        {/each}
        {#if found.organisations.length > 0}
          <div class="orgs" role="group" aria-label="Organisations to cover">
            <span class="hint">Cover an organisation:</span>
            {#each found.organisations as organisation (organisation)}
              <button
                type="button"
                class="btn chipbtn"
                aria-pressed={hasOwner(ownersText, organisation)}
                onclick={() =>
                  (ownersText = toggleOwner(ownersText, organisation))}
                >{organisation}</button
              >
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  {/if}

  <div class="field">
    <label for="{id}-owners"
      >Owners or organisations it covers on {coverHost}</label
    >
    <input
      id="{id}-owners"
      type="text"
      autocomplete="off"
      autocapitalize="off"
      spellcheck="false"
      placeholder="acme, acme-labs"
      bind:value={ownersText}
      aria-invalid={problem?.field === "owners" ? "true" : undefined}
      aria-describedby={problem?.field === "owners" ? `${id}-error` : undefined}
    />
    <label class="choice">
      <input type="checkbox" bind:checked={rest} />
      The rest of {coverHost}
    </label>
    <p class="hint">
      A request to an owner you name uses this credential; the rest of the host
      applies to every other owner.
    </p>
    <p class="hint" id="{id}-decrypt-hint">
      Puddle will decrypt connections to {coverHost} (port 443 only), so it can add
      the credential to Git requests there. Puddle's own certificate stands in for
      the host's inside the workspace.
    </p>
  </div>

  {#if problem}
    <p class="error" id="{id}-error" role="alert">{problem.message}</p>
  {/if}
  <div class="actions">
    <button type="button" class="btn" onclick={onCancel}>Cancel</button>
    <button type="submit" class="btn primary" disabled={busy}
      >Add credential</button
    >
  </div>
</form>

<style>
  .editor {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-3);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  fieldset {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    border: 0;
  }
  legend {
    padding: 0;
    margin-bottom: var(--space-1);
    font-weight: 600;
  }
  .found {
    display: grid;
    gap: var(--space-1);
    margin-inline-start: var(--space-6);
  }
  .field {
    display: grid;
    gap: var(--space-1);
  }
  .field > label:first-child {
    font-weight: 600;
  }
  .choice {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  input[type="text"],
  input[type="password"] {
    min-height: var(--control-size);
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font: inherit;
  }
  input[aria-invalid="true"] {
    border-color: var(--color-danger);
  }
  .hint {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .error {
    margin: 0;
    color: var(--color-danger);
    font-size: var(--text-sm);
  }
  .profile {
    display: grid;
    gap: var(--space-1);
  }
  .orgs {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  .chipbtn[aria-pressed="true"] {
    border-color: var(--color-accent);
    font-weight: 600;
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-pill);
    color: var(--color-warning);
    font-size: var(--text-sm);
    white-space: nowrap;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
