<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { untrack } from "svelte";
  import {
    coverageProblem,
    parseOwners,
    type Coverage,
  } from "#lib/identities/model.ts";
  import "#lib/theme/controls.css";

  // Changes what a credential already on the identity covers. The credential itself, and the token
  // behind it, stay as they are: only the owners and the rest of the host change.
  let {
    host,
    covers: current,
    onSave,
    onCancel,
  }: {
    host: string;
    covers: Coverage;
    onSave: (covers: Coverage) => void;
    onCancel: () => void;
  } = $props();

  const id = $props.id();
  let ownersText = $state(untrack(() => current.owners.join(", ")));
  let rest = $state(untrack(() => current.rest_of_host));
  let problem = $state<string | null>(null);

  function submit(event: SubmitEvent) {
    event.preventDefault();
    const owners = parseOwners(ownersText);
    if (!owners.ok) {
      problem = owners.message;
      return;
    }
    const covers = { owners: owners.owners, rest_of_host: rest };
    problem = coverageProblem(covers);
    if (problem === null) onSave(covers);
  }
</script>

<form
  class="coverage"
  onsubmit={submit}
  novalidate
  aria-label="Change coverage"
>
  <div class="field">
    <label for="{id}-owners">
      Owners or organisations it covers on {host}
    </label>
    <input
      id="{id}-owners"
      type="text"
      autocomplete="off"
      autocapitalize="off"
      spellcheck="false"
      placeholder="acme, acme-labs"
      bind:value={ownersText}
      aria-invalid={problem ? "true" : undefined}
      aria-describedby={problem ? `${id}-error` : undefined}
    />
    <label class="choice">
      <input type="checkbox" bind:checked={rest} />
      The rest of {host}
    </label>
  </div>
  {#if problem}
    <p class="error" id="{id}-error" role="alert">{problem}</p>
  {/if}
  <div class="actions">
    <button type="button" class="btn" onclick={onCancel}>Cancel</button>
    <button type="submit" class="btn primary">Change coverage</button>
  </div>
</form>

<style>
  .coverage {
    display: grid;
    flex-basis: 100%;
    gap: var(--space-2);
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
  .error {
    margin: 0;
    color: var(--color-danger);
    font-size: var(--text-sm);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
