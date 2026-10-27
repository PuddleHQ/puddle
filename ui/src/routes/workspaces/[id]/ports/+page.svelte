<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { page } from "$app/state";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import { WorkspacePortsStore } from "#lib/stores/workspace-ports.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import {
    forwardText,
    hasForward,
    parsePort,
    type PortEntry,
  } from "#lib/workspaces/ports.ts";
  import "#lib/theme/controls.css";

  const id = $derived(page.params["id"] ?? "");
  const workspace = $derived(workspaces.list.find((w) => w.id === id));
  const name = $derived(workspace?.name);
  const store = new WorkspacePortsStore();
  const fid = $props.id();

  // The port on this computer typed for a row, by the workspace's port; and the "another port" form.
  let hostFor = $state<Record<number, string>>({});
  let otherGuest = $state("");
  let otherHost = $state("");
  let otherKeep = $state(false);
  let problem = $state<string | null>(null);
  let formProblem = $state<string | null>(null);
  let busy = $state(false);

  $effect(() => {
    if (name) return store.start(name);
  });

  async function forward(
    guest: number,
    hostText: string,
    pinned: boolean,
  ): Promise<boolean> {
    const host = hostText.trim() === "" ? null : parsePort(hostText);
    if (hostText.trim() !== "" && host === null) {
      problem = `${hostText.trim()} is not a port: use a number from 1 to 65535.`;
      return false;
    }
    busy = true;
    const result = await store.forward(guest, host, pinned);
    busy = false;
    if (!result.ok) {
      problem = result.message;
      return false;
    }
    toasts.push(
      `${guest} is forwarded to 127.0.0.1:${host ?? guest}${pinned ? " and kept" : ""}.`,
    );
    return true;
  }

  async function onForward(row: PortEntry, pinned: boolean) {
    problem = null;
    if (await forward(row.guest_port, hostFor[row.guest_port] ?? "", pinned)) {
      delete hostFor[row.guest_port];
    }
  }

  async function onKeep(row: PortEntry, pinned: boolean) {
    problem = null;
    // Asking again for the same forward changes only whether it is kept.
    busy = true;
    const result = await store.forward(row.guest_port, row.host_port, pinned);
    busy = false;
    if (!result.ok) problem = result.message;
  }

  async function onStop(row: PortEntry) {
    problem = null;
    busy = true;
    const result = await store.release(row.guest_port);
    busy = false;
    if (!result.ok) problem = result.message;
    else toasts.push(`Stopped forwarding ${row.guest_port}.`);
  }

  async function other(event: SubmitEvent) {
    event.preventDefault();
    formProblem = null;
    const guest = parsePort(otherGuest);
    if (guest === null) {
      formProblem = "Give the workspace's port as a number from 1 to 65535.";
      document.getElementById(`${fid}-guest`)?.focus();
      return;
    }
    problem = null;
    if (await forward(guest, otherHost, otherKeep)) {
      otherGuest = "";
      otherHost = "";
      otherKeep = false;
    }
  }
</script>

{#if workspace && store.status === "ready"}
  <section aria-labelledby="{fid}-h">
    <h2 id="{fid}-h">Ports</h2>
    <p class="muted">
      The ports programs in this workspace listen on, as the workspace reports
      them. Forward one to reach it from this computer: the forward is open on
      127.0.0.1 only, never on your network. Keep a forward and it comes back at
      every start of the workspace.
    </p>
    {#if !store.running}
      <p class="warn" role="status">
        {workspace.display_name} is not running, so nothing listens and a port can't
        be forwarded yet. Forwards you keep are listed and open again when it starts.
      </p>
    {/if}

    {#if store.ports.length === 0}
      <p class="muted">
        {store.running
          ? "Nothing listens in the workspace and no forward is kept."
          : "No forward is kept."}
      </p>
    {:else}
      <div class="wrap">
        <table aria-label="Ports of {workspace.name}">
          <thead>
            <tr>
              <th scope="col">Port</th>
              <th scope="col">Listening</th>
              <th scope="col">On this computer</th>
              <th scope="col">Kept</th>
              <th scope="col"><span class="visually-hidden">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each store.ports as row (row.guest_port)}
              <tr data-port={row.guest_port}>
                <td class="mono">{row.guest_port}</td>
                <td>{row.listening ? "Yes" : "No"}</td>
                <td>
                  {#if hasForward(row)}
                    {forwardText(row)}
                  {:else}
                    <input
                      type="text"
                      inputmode="numeric"
                      class="host"
                      autocomplete="off"
                      placeholder={String(row.guest_port)}
                      aria-label="Port on this computer for {row.guest_port}"
                      disabled={!store.running}
                      bind:value={hostFor[row.guest_port]}
                    />
                  {/if}
                  {#if row.problem}
                    <p class="error" role="alert">{row.problem}</p>
                  {/if}
                </td>
                <td>
                  {#if hasForward(row)}
                    <input
                      type="checkbox"
                      aria-label="Keep the forward of {row.guest_port} at every start"
                      checked={row.pinned}
                      disabled={busy || !row.forwarded}
                      onchange={(e) => {
                        const next = e.currentTarget.checked;
                        e.currentTarget.checked = row.pinned;
                        void onKeep(row, next);
                      }}
                    />
                  {/if}
                </td>
                <td class="actions">
                  {#if hasForward(row)}
                    <button
                      type="button"
                      class="btn"
                      disabled={busy}
                      aria-label="Stop forwarding {row.guest_port}"
                      onclick={() => onStop(row)}>Stop</button
                    >
                  {:else}
                    <button
                      type="button"
                      class="btn"
                      disabled={busy || !store.running}
                      aria-label="Forward {row.guest_port}"
                      onclick={() => onForward(row, false)}>Forward</button
                    >
                    <button
                      type="button"
                      class="btn"
                      disabled={busy || !store.running}
                      aria-label="Forward {row.guest_port} and keep it"
                      onclick={() => onForward(row, true)}
                      >Forward and keep</button
                    >
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
    {#if problem}
      <p class="error" role="alert">{problem}</p>
    {/if}

    <form class="add" onsubmit={other} novalidate>
      <div class="field">
        <label for="{fid}-guest">Forward another port</label>
        <input
          id="{fid}-guest"
          type="text"
          inputmode="numeric"
          autocomplete="off"
          placeholder="Port in the workspace"
          bind:value={otherGuest}
          aria-invalid={formProblem ? "true" : undefined}
          aria-describedby={formProblem ? `${fid}-form-error` : undefined}
        />
      </div>
      <div class="field">
        <label for="{fid}-host">On this computer</label>
        <input
          id="{fid}-host"
          type="text"
          inputmode="numeric"
          autocomplete="off"
          placeholder="Same number"
          bind:value={otherHost}
        />
      </div>
      <label class="choice"
        ><input type="checkbox" bind:checked={otherKeep} />Keep it</label
      >
      <button type="submit" class="btn" disabled={busy || !store.running}
        >Forward</button
      >
    </form>
    {#if formProblem}
      <p class="error" id="{fid}-form-error" role="alert">{formProblem}</p>
    {/if}
  </section>
{:else if store.status === "failed"}
  <p class="muted">Couldn't read the ports yet. Trying again.</p>
{:else}
  <p class="muted">Loading ports&hellip;</p>
{/if}

<style>
  section {
    display: grid;
    gap: var(--space-3);
    margin-bottom: var(--space-6);
    max-width: 60rem;
  }
  h2 {
    font-size: var(--text-lg);
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--color-text-muted);
  }
  .warn {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
  }
  .error {
    color: var(--color-danger);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
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
  .actions {
    text-align: end;
    white-space: nowrap;
  }
  .add {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: var(--space-3);
  }
  .field {
    display: grid;
    gap: var(--space-1);
  }
  .field label {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  input[type="text"] {
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
  input.host {
    width: 7rem;
  }
  .choice {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--control-size);
  }
</style>
