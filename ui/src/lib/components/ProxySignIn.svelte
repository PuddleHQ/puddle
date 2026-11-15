<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { ProxySignInStore } from "#lib/stores/proxy-sign-in.svelte.ts";
  import "#lib/theme/controls.css";

  // A user name and password for a company proxy that asks for them (Basic). Puddle tries the
  // Windows sign-in first and uses these when the proxy still asks. The password is kept in the
  // computer's credential store and is never shown again.
  const store = new ProxySignInStore();
  const id = $props.id();
  let proxy = $state("");
  let user = $state("");
  let password = $state("");
  let failure = $state<string | null>(null);
  let saved = $state(false);
  let working = $state(false);

  $effect(() => {
    void store.load();
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    working = true;
    failure = null;
    saved = false;
    const result = await store.save(proxy.trim(), user, password);
    working = false;
    if (result.ok) {
      saved = true;
      // Nothing keeps the password once it is stored.
      password = "";
      user = "";
      proxy = "";
    } else {
      failure = result.message;
    }
  }

  async function forget(name: string) {
    failure = null;
    saved = false;
    const result = await store.remove(name);
    if (!result.ok) failure = result.message;
  }
</script>

<h3 id="{id}-title">Proxy sign-in</h3>
{#if store.status === "failed"}
  <div role="alert">
    <p class="muted">Couldn't read the proxy sign-ins.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else if store.status === "ready"}
  <p class="desc">
    For a company proxy that asks for a user name and password. Puddle signs in
    with your Windows account first, as before, and uses these when the proxy
    still asks. The password is kept in this computer's credential store and
    isn't shown again.
  </p>
  {#if store.signIns.length > 0}
    <ul class="list" aria-label="Stored proxy sign-ins">
      {#each store.signIns as signIn (signIn.proxy)}
        <li>
          <span class="mono">{signIn.proxy}</span>
          <span class="muted">signs in as {signIn.user}</span>
          <button
            type="button"
            class="btn"
            aria-label="Remove the sign-in for {signIn.proxy}"
            onclick={() => forget(signIn.proxy)}>Remove</button
          >
        </li>
      {/each}
    </ul>
  {:else}
    <p class="muted">No proxy sign-in is stored.</p>
  {/if}
  <form onsubmit={submit} autocomplete="off">
    <div class="row">
      <label for="{id}-proxy">Proxy</label>
      <input
        id="{id}-proxy"
        type="text"
        list="{id}-proxies"
        placeholder="proxy.example.com:8080"
        bind:value={proxy}
        oninput={() => (saved = false)}
        required
      />
      <datalist id="{id}-proxies">
        {#each store.suggestions as name (name)}
          <option value={name}></option>
        {/each}
      </datalist>
      <label for="{id}-user">User name</label>
      <input
        id="{id}-user"
        type="text"
        autocomplete="off"
        bind:value={user}
        oninput={() => (saved = false)}
        required
      />
      <label for="{id}-password">Password</label>
      <input
        id="{id}-password"
        type="password"
        autocomplete="new-password"
        bind:value={password}
        oninput={() => (saved = false)}
        required
      />
    </div>
    <div class="row">
      <button type="submit" class="btn primary" disabled={working}
        >Keep sign-in</button
      >
      <span aria-live="polite">
        {#if saved}<span class="saved">Saved.</span>{/if}
        {#if failure}<span class="error" role="alert">{failure}</span>{/if}
      </span>
    </div>
  </form>
{/if}

<style>
  form {
    display: grid;
    gap: var(--space-2);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
  }
  .list {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .list li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
  }
  label {
    font-weight: 600;
  }
  input {
    width: 12rem;
    min-height: var(--control-size);
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font: inherit;
  }
  .desc,
  .muted {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .saved {
    color: var(--color-success);
  }
  .error {
    margin: 0;
    color: var(--color-danger);
  }
</style>
