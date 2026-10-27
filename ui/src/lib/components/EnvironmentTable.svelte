<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { AlertDialog } from "bits-ui";
  import {
    hostsSummary,
    ordered,
    PLAIN_AT_NEXT_START,
    SECRET_AT_ONCE,
    type EnvSet,
    type EnvVariable,
  } from "#lib/environment/model.ts";
  import type { EnvironmentStore } from "#lib/stores/environment.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import EnvironmentDialog from "./EnvironmentDialog.svelte";
  import "#lib/theme/controls.css";

  // The variables and secrets of one scope, with add, edit and remove. A secret row never shows a
  // value (Puddle does not read it back): it says "Stored in Puddle" and names the hosts it is sent
  // to. In a workspace the global entries it inherits are listed after its own, and one of them
  // can be overridden here.
  let {
    store,
    label,
    workspace = null,
    running = false,
  }: {
    store: EnvironmentStore;
    /** What the table is called for assistive technology: "Environment of web-shop". */
    label: string;
    /** The workspace this table is for; `null` is the global list. */
    workspace?: string | null;
    running?: boolean;
  } = $props();

  const where = $derived(workspace ?? "every workspace");
  const rows = $derived(ordered(store.variables));

  let dialogOpen = $state(false);
  let editing = $state.raw<EnvVariable | null>(null);
  let presetName = $state("");
  let removing = $state.raw<EnvVariable | null>(null);
  let removeOpen = $state(false);
  let rowProblem = $state<string | null>(null);
  let applied = $state<string | null>(null);

  function add(preset = "") {
    editing = null;
    presetName = preset;
    dialogOpen = true;
  }

  function edit(row: EnvVariable) {
    editing = row;
    presetName = "";
    dialogOpen = true;
  }

  async function save(name: string, body: EnvSet): Promise<string | null> {
    const result = await store.set(name, body);
    if (!result.ok) return result.message;
    toasts.push(`Saved ${name}.`);
    applied =
      body.kind === "secret"
        ? `${name}: ${SECRET_AT_ONCE}`
        : workspace !== null && running
          ? `${name}: ${PLAIN_AT_NEXT_START}`
          : null;
    return null;
  }

  async function remove(row: EnvVariable) {
    rowProblem = null;
    applied = null;
    const result = await store.remove(row.name);
    if (!result.ok) {
      rowProblem = result.message;
      return;
    }
    toasts.push(`Removed ${row.name}.`);
  }

  function ask(row: EnvVariable) {
    if (row.kind === "plain") {
      void remove(row);
      return;
    }
    removing = row;
    removeOpen = true;
  }
</script>

<div class="bar">
  <button type="button" class="btn primary" onclick={() => add()}>
    Add variable or secret
  </button>
</div>

{#if store.status === "failed"}
  <p class="muted">Couldn't read the variables yet. Trying again.</p>
{:else if store.status === "loading"}
  <p class="muted">Loading variables&hellip;</p>
{:else if rows.length === 0}
  <p class="muted">
    {workspace === null
      ? "No variable is set for every workspace."
      : "This workspace has no variables of its own, and no global ones reach it."}
  </p>
{:else}
  <div class="wrap">
    <table aria-label={label}>
      <thead>
        <tr>
          <th scope="col">Name</th>
          <th scope="col">Value</th>
          <th scope="col">Applies to</th>
          <th scope="col"><span class="visually-hidden">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each rows as row (row.scope + row.name)}
          <tr
            data-name={row.name}
            data-scope={row.scope}
            class:hidden={row.overridden}
          >
            <td class="mono name">{row.name}</td>
            <td class="value">
              {#if row.kind === "secret"}
                <span class="chip">Secret</span>
                Stored in Puddle, never shown. Sent only to
                <span class="mono">{hostsSummary(row.hosts)}</span>.
              {:else}
                <span class="mono">{row.value}</span>
              {/if}
              {#if row.overridden}
                <span class="muted"
                  >Hidden here by this workspace's own {row.name}.</span
                >
              {/if}
            </td>
            <td>
              {row.scope === "global" && workspace !== null
                ? "Every workspace"
                : where}
            </td>
            <td class="actions">
              {#if row.scope === "global" && workspace !== null}
                <button
                  type="button"
                  class="btn"
                  aria-label="Override {row.name} for {workspace}"
                  onclick={() => add(row.name)}>Override</button
                >
              {:else}
                <button
                  type="button"
                  class="btn"
                  aria-label="Edit {row.name}"
                  onclick={() => edit(row)}>Edit</button
                >
                <button
                  type="button"
                  class="btn"
                  aria-label="Remove {row.name}"
                  onclick={() => ask(row)}>Remove</button
                >
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}
{#if rowProblem}
  <p class="error" role="alert">{rowProblem}</p>
{/if}
{#if applied}
  <p class="muted" role="status">{applied}</p>
{/if}

<EnvironmentDialog
  bind:open={dialogOpen}
  {editing}
  name={presetName}
  {where}
  onSubmit={save}
/>

<AlertDialog.Root bind:open={removeOpen}>
  <AlertDialog.Portal>
    <AlertDialog.Overlay class="confirm-overlay" />
    <AlertDialog.Content class="confirm-content">
      <AlertDialog.Title class="confirm-title"
        >Remove {removing?.name}?</AlertDialog.Title
      >
      <AlertDialog.Description class="confirm-detail">
        Puddle deletes the value from this computer's credential store. It can't
        be shown again, so you would have to type it in again to use the secret.
      </AlertDialog.Description>
      <div class="confirm-actions">
        <AlertDialog.Cancel class="btn">Cancel</AlertDialog.Cancel>
        <AlertDialog.Action
          class="btn deny"
          onclick={() => {
            if (removing) void remove(removing);
          }}>Remove secret</AlertDialog.Action
        >
      </div>
    </AlertDialog.Content>
  </AlertDialog.Portal>
</AlertDialog.Root>

<style>
  .bar {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--color-text-muted);
  }
  .error {
    color: var(--color-danger);
    font-size: var(--text-sm);
  }
  .wrap {
    overflow-x: auto;
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  th,
  td {
    padding: var(--space-1) var(--space-3);
    text-align: start;
    border-bottom: 1px solid var(--color-border-subtle);
    vertical-align: middle;
  }
  tbody tr:last-child td {
    border-bottom: 0;
  }
  tr.hidden .name,
  tr.hidden .value {
    color: var(--color-text-muted);
  }
  .name,
  .value {
    overflow-wrap: anywhere;
  }
  .chip {
    display: inline-block;
    padding: 0 var(--space-2);
    margin-inline-end: var(--space-1);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
  }
  .actions {
    text-align: end;
    white-space: nowrap;
  }
</style>
