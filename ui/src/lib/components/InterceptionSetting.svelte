<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import LearnedHostsList from "#lib/components/LearnedHostsList.svelte";
  import NeverDecryptEditor from "#lib/components/NeverDecryptEditor.svelte";
  import { WorkspaceInterception } from "#lib/stores/interception.svelte.ts";
  import "#lib/theme/controls.css";

  // Whether Puddle decrypts every HTTPS host of one workspace, not only the hosts with a
  // credential, and the names it leaves alone either way. The workspace applies a change at once.
  let { id }: { id: string } = $props();

  const store = new WorkspaceInterception();
  const headingId = $props.id();
  let problem = $state<string | null>(null);
  let saved = $state<string | null>(null);

  $effect(() => {
    void store.load(id);
  });

  async function toggle(box: HTMLInputElement) {
    problem = null;
    saved = null;
    const result = await store.setDecryptAll(box.checked);
    if (result.ok) {
      saved = store.decryptAll
        ? "Puddle now decrypts every host."
        : "Puddle decrypts only hosts with a credential.";
    } else {
      problem = result.message;
      box.checked = store.decryptAll;
    }
  }
</script>

<section class="card" aria-labelledby={headingId}>
  <h2 id={headingId}>Decrypt every host</h2>
  {#if store.status === "loading"}
    <p class="muted">Loading&hellip;</p>
  {:else if store.status === "failed"}
    <div role="alert">
      <p class="muted">Couldn't read this setting.</p>
      {#if store.reason}<p class="error">{store.reason}</p>{/if}
    </div>
  {:else}
    <div class="setting">
      <div class="grow">
        <label for="ws-decrypt-all">Decrypt every host</label>
        <p class="desc" id="ws-decrypt-all-desc">
          By default Puddle decrypts only the hosts it has a credential, secret
          or login capture for. With this on it decrypts every HTTPS host, so
          rules can see methods and paths. A tool that doesn't trust Puddle's
          certificate fails once for that host, and Puddle then leaves the host
          alone for 12 hours (the audit log shows it as pass_through:learned).
          Connections that aren't HTTPS, servers that ask for a client
          certificate and servers whose certificate Puddle doesn't accept go
          through untouched. It applies at once, no restart.
        </p>
      </div>
      <input
        id="ws-decrypt-all"
        type="checkbox"
        role="switch"
        aria-describedby="ws-decrypt-all-desc"
        checked={store.decryptAll}
        onchange={(e) => void toggle(e.currentTarget)}
      />
    </div>
    <div class="status" aria-live="polite">
      {#if saved}<p class="saved">{saved}</p>{/if}
      {#if problem}<p class="error" role="alert">{problem}</p>{/if}
    </div>
    <NeverDecryptEditor
      label="Never decrypt in this workspace"
      hint="One host name per line, or *. and a domain such as *.example.com. These stay untouched while the switch is on. Hosts with a credential are always decrypted."
      names={store.names}
      save={(names) => store.saveNames(names)}
    />
    {#if store.globalNames.length > 0}
      <p class="desc">
        Never decrypted in any workspace (set in
        <a href="/settings#s-net">global settings</a>):
        {store.globalNames.join(", ")}
      </p>
    {/if}
    <LearnedHostsList {id} />
  {/if}
</section>

<style>
  .card {
    display: grid;
    gap: var(--space-3);
    max-width: 48rem;
    margin-bottom: var(--space-4);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  h2 {
    font-size: var(--text-md);
  }
  .setting {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  label {
    font-weight: 600;
  }
  .desc,
  .muted {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .status p {
    margin: 0;
  }
  .saved {
    color: var(--color-success);
  }
  .error {
    color: var(--color-danger);
  }
  @media (max-width: 760px) {
    .setting {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
