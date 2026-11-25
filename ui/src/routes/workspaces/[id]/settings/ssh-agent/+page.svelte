<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { page } from "$app/state";
  import { parseKeyList } from "#lib/workspaces/ssh-agent-keys.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import { WorkspaceSshAgentStore } from "#lib/stores/workspace-ssh-agent.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import "#lib/theme/controls.css";

  const id = $derived(page.params["id"] ?? "");
  const workspace = $derived(workspaces.list.find((w) => w.id === id));
  const wid = $derived(workspace?.id);
  const store = new WorkspaceSshAgentStore();
  const fid = $props.id();

  let host = $state("");
  let keysText = $state("");
  let problem = $state<string | null>(null);
  let busy = $state(false);

  $effect(() => {
    if (wid) return store.start(wid);
  });

  async function turnOn(event: SubmitEvent) {
    event.preventDefault();
    if (busy) return;
    const name = host.trim();
    if (name === "") {
      problem = "Name the host the workspace logs in to.";
      return;
    }
    const keys = parseKeyList(keysText);
    if (!keys.ok) {
      problem = keys.message;
      return;
    }
    busy = true;
    problem = null;
    const result = await store.turnOn(name, keys.keys);
    busy = false;
    if (!result.ok) {
      problem = result.message;
      return;
    }
    host = "";
    keysText = "";
    toasts.push(`Turned the SSH agent on for ${name}.`);
  }

  async function turnOff(name: string) {
    if (busy) return;
    busy = true;
    const result = await store.turnOff(name);
    busy = false;
    if (!result.ok) {
      toasts.push(result.message, { tone: "error", ms: 8000 });
      return;
    }
    toasts.push(`Turned the SSH agent off for ${name}.`);
  }
</script>

{#if workspace && store.status === "ready"}
  <section aria-labelledby="{fid}-h">
    <h2 id="{fid}-h">SSH agent</h2>
    <p class="muted">
      Lets <span class="mono">ssh</span> in this workspace log in to a host with a
      key from your own SSH agent, without the key leaving this computer. It is off
      for every host until you turn it on for one here. Puddle signs only the login
      to that host, only for a connection it saw the workspace open there, and the
      first time for each key it asks you in the Inbox. It never signs commits, and
      never for a Git host Puddle already handles.
    </p>

    {#if store.hosts.length === 0}
      <p class="muted">Off for every host.</p>
    {:else}
      <ul class="hosts">
        {#each store.hosts as entry (entry.host)}
          <li>
            <span class="host mono">{entry.host}</span>
            <span class="keys">
              {#if entry.keys === null}
                every key in your agent
              {:else}
                {entry.keys.length === 1
                  ? "1 key"
                  : `${entry.keys.length} keys`}:
                <span class="mono fp">{entry.keys.join(", ")}</span>
              {/if}
            </span>
            <button
              type="button"
              class="btn deny"
              disabled={busy}
              aria-label="Turn off the SSH agent for {entry.host}"
              onclick={() => void turnOff(entry.host)}>Turn off</button
            >
          </li>
        {/each}
      </ul>
    {/if}

    <form onsubmit={turnOn} novalidate>
      <h3>Turn on for a host</h3>
      <div class="field">
        <label for="{fid}-host">Host</label>
        <input
          id="{fid}-host"
          class="mono input-wide"
          type="text"
          spellcheck="false"
          autocapitalize="off"
          autocomplete="off"
          placeholder="build.example.com"
          bind:value={host}
          aria-describedby="{fid}-host-hint"
        />
        <p class="hint" id="{fid}-host-hint">
          The name or address <span class="mono">ssh</span> in the workspace connects
          to.
        </p>
      </div>
      <div class="field">
        <label for="{fid}-keys">Only these keys (optional)</label>
        <textarea
          id="{fid}-keys"
          class="mono input-wide"
          rows="3"
          spellcheck="false"
          autocapitalize="off"
          placeholder="SHA256:..."
          bind:value={keysText}
          aria-describedby={problem ? `${fid}-error` : `${fid}-keys-hint`}
          aria-invalid={problem ? "true" : undefined}></textarea>
        <p class="hint" id="{fid}-keys-hint">
          One fingerprint per line, as <span class="mono">ssh-keygen -l</span> prints
          them. Leave it empty to allow every key in your agent.
        </p>
      </div>
      {#if problem}
        <p class="error" id="{fid}-error" role="alert">{problem}</p>
      {/if}
      <div class="acts">
        <button type="submit" class="btn primary" disabled={busy}
          >Turn on</button
        >
      </div>
    </form>
  </section>
{:else if store.status === "failed"}
  <p class="muted">Couldn't read the SSH agent settings yet. Trying again.</p>
{:else}
  <p class="muted">Loading the SSH agent settings&hellip;</p>
{/if}

<style>
  section {
    max-width: var(--page-reading-width);
    display: grid;
    gap: var(--space-3);
    margin-bottom: var(--space-6);
  }
  h2 {
    font-size: var(--text-lg);
  }
  h3 {
    font-size: var(--text-md);
  }
  p {
    margin: 0;
  }
  .muted,
  .hint {
    color: var(--color-text-muted);
  }
  .hint {
    font-size: var(--text-sm);
  }
  .error {
    color: var(--color-danger);
  }
  .hosts {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .hosts li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--color-border-subtle);
  }
  .hosts li:last-child {
    border-bottom: 0;
  }
  .host {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .keys {
    flex: 1;
    min-width: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .fp {
    overflow-wrap: anywhere;
  }
  form {
    display: grid;
    gap: var(--space-3);
  }
  .field {
    display: grid;
    gap: var(--space-1);
  }
  .field label {
    font-weight: 600;
  }

  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
</style>
