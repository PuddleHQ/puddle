<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { untrack } from "svelte";
  import {
    DECRYPT_HOSTS,
    hostsText,
    nameProblem,
    parseHosts,
    PLACE_HINT,
    placeOf,
    SECRET_NEVER_IN,
    SWAP_LIMITS,
    type EnvSet,
    type EnvVariable,
    type PlaceChoice,
  } from "#lib/environment/model.ts";
  import FormDialog from "./FormDialog.svelte";

  // Adds a variable or a secret, or edits one: a plain variable's value, or a secret's hosts, the
  // place in a request it is swapped in, and (when typed) its new value. A secret's current value is not known here and is never asked back.
  // `onSubmit` returns the service's refusal as text, or `null` when it went through.
  let {
    open = $bindable(false),
    editing = null,
    name: presetName = "",
    where,
    onSubmit,
  }: {
    open?: boolean;
    /** The variable being edited; `null` adds one. */
    editing?: EnvVariable | null;
    /** A name to start with when adding (overriding a global one). */
    name?: string;
    /** Where the variable applies, in words: "web-shop" or "every workspace". */
    where: string;
    onSubmit: (name: string, body: EnvSet) => Promise<string | null>;
  } = $props();

  const id = $props.id();
  let name = $state("");
  let kind = $state<"plain" | "secret">("plain");
  let value = $state("");
  let hosts = $state("");
  let place = $state<PlaceChoice>("header");
  let queryName = $state("");
  let refusal = $state<string | null>(null);
  let busy = $state(false);

  const problem = $derived(refusal);
  const keepsValue = $derived(editing?.kind === "secret");

  // Start fresh each time the dialog opens, not each time the live row is replaced.
  let wasOpen = false;
  $effect(() => {
    if (open && !wasOpen) {
      untrack(() => {
        name = editing?.name ?? presetName;
        kind = editing?.kind ?? "plain";
        value = editing?.kind === "plain" ? (editing.value ?? "") : "";
        hosts = editing ? hostsText(editing.hosts) : "";
        place = editing?.place?.in ?? "header";
        queryName = editing?.place?.in === "query" ? editing.place.name : "";
        refusal = null;
      });
    } else if (!open && wasOpen) {
      // A typed secret must not outlive the dialog, whether it was saved, refused or cancelled.
      untrack(() => {
        value = "";
      });
    }
    wasOpen = open;
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    const bad = editing ? null : nameProblem(name.trim());
    if (bad) {
      refusal = bad;
      return;
    }
    let body: EnvSet;
    if (kind === "plain") {
      body = { kind: "plain", value };
    } else {
      const list = parseHosts(hosts);
      if (list.length === 0) {
        refusal = "Name at least one host the secret may be sent to.";
        return;
      }
      const swappedIn = placeOf(place, queryName);
      if ("problem" in swappedIn) {
        refusal = swappedIn.problem;
        return;
      }
      if (value === "" && !keepsValue) {
        refusal = "A new secret needs its value.";
        return;
      }
      body = {
        kind: "secret",
        hosts: list,
        place: swappedIn,
        ...(value === "" ? {} : { value }),
      };
    }
    busy = true;
    try {
      refusal = await onSubmit(editing?.name ?? name.trim(), body);
      if (refusal === null) open = false;
    } finally {
      busy = false;
    }
  }
</script>

<FormDialog
  bind:open
  title={editing
    ? `Edit ${editing.name}`
    : kind === "secret"
      ? "Add a secret"
      : "Add a variable"}
  description={`Applies to ${where}.`}
>
  <form onsubmit={submit} novalidate>
    <div class="field">
      <label for="{id}-name">Name</label>
      <input
        id="{id}-name"
        type="text"
        class="mono"
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        placeholder="MY_TOKEN"
        disabled={editing !== null}
        bind:value={name}
        oninput={() => (refusal = null)}
        aria-invalid={problem ? "true" : undefined}
        aria-describedby={problem ? `${id}-error` : undefined}
      />
    </div>
    {#if !editing}
      <fieldset>
        <legend>What is it</legend>
        <label class="choice">
          <input type="radio" bind:group={kind} value="plain" />
          Variable: the workspace sees its value
        </label>
        <label class="choice">
          <input type="radio" bind:group={kind} value="secret" />
          Secret: the workspace never sees its value
        </label>
      </fieldset>
    {/if}
    <div class="field">
      <label for="{id}-value">Value</label>
      {#if kind === "secret"}
        <input
          id="{id}-value"
          type="password"
          class="mono wide"
          autocomplete="off"
          spellcheck="false"
          bind:value
          oninput={() => (refusal = null)}
          aria-describedby="{id}-value-hint"
        />
        <p class="hint" id="{id}-value-hint">
          {keepsValue
            ? "Leave it empty to keep the value Puddle holds; it is never shown again."
            : "Written once and never shown again."}
          {SECRET_NEVER_IN}
        </p>
      {:else}
        <textarea
          id="{id}-value"
          class="mono wide"
          rows="3"
          spellcheck="false"
          bind:value
          oninput={() => (refusal = null)}></textarea>
      {/if}
    </div>
    {#if kind === "secret"}
      <div class="field">
        <label for="{id}-hosts">Hosts it may be sent to</label>
        <textarea
          id="{id}-hosts"
          class="mono wide"
          rows="3"
          autocapitalize="off"
          spellcheck="false"
          placeholder="api.example.com&#10;*.example.org"
          bind:value={hosts}
          oninput={() => (refusal = null)}
          aria-describedby="{id}-hosts-hint"></textarea>
        <p class="hint" id="{id}-hosts-hint">
          One per line: a name, or *.name for every name below it.
          {DECRYPT_HOSTS}
        </p>
      </div>
      <fieldset aria-describedby="{id}-place-hint">
        <legend>Where the tool sends it</legend>
        <label class="choice">
          <input type="radio" bind:group={place} value="header" />
          In a header (most tools)
        </label>
        <label class="choice">
          <input type="radio" bind:group={place} value="query" />
          In a query parameter
        </label>
        {#if place === "query"}
          <div class="field">
            <label for="{id}-query">Parameter name</label>
            <input
              id="{id}-query"
              type="text"
              class="mono"
              autocomplete="off"
              autocapitalize="off"
              spellcheck="false"
              placeholder="appid"
              bind:value={queryName}
              oninput={() => (refusal = null)}
            />
          </div>
        {/if}
        <label class="choice">
          <input type="radio" bind:group={place} value="path" />
          In the path of the URL
        </label>
        <p class="hint" id="{id}-place-hint">{PLACE_HINT}</p>
      </fieldset>
      <p class="hint" role="note">{SWAP_LIMITS}</p>
    {/if}
    {#if problem}
      <p class="error" id="{id}-error" role="alert">{problem}</p>
    {/if}
    <div class="actions">
      <button type="button" class="btn" onclick={() => (open = false)}
        >Cancel</button
      >
      <button type="submit" class="btn primary" disabled={busy}
        >{editing ? "Save" : "Add"}</button
      >
    </div>
  </form>
</FormDialog>

<style>
  :global(.form-dialog textarea.wide),
  :global(.form-dialog input.wide),
  :global(.form-dialog input.mono[type="text"]) {
    box-sizing: border-box;
    width: 100%;
  }
  :global(.form-dialog textarea),
  :global(.form-dialog input[type="password"]) {
    min-height: var(--control-size);
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font-size: inherit;
    resize: vertical;
  }
</style>
