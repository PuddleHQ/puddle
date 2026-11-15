<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { absoluteTime, relativeTime } from "#lib/format/relative-time.ts";
  import {
    MIN_PASSWORD_CHARS,
    type SessionStore,
  } from "#lib/session/session.svelte.ts";
  import type { DevicesStore } from "#lib/stores/devices.svelte.ts";
  import "#lib/theme/controls.css";

  // The browsers signed in as this user, each with a way to sign it out; the password; and, for the
  // owner, which ways of signing in are on. Nothing here shows a credential.
  let { session, store }: { session: SessionStore; store: DevicesStore } =
    $props();

  const id = $props.id();
  let problem = $state<string | null>(null);
  let current = $state("");
  let next = $state("");
  let again = $state("");
  let changed = $state(false);
  let passwordProblem = $state<string | null>(null);

  $effect(() => {
    void store.load();
  });

  const passwordOn = $derived(
    store.methods.find((m) => m.method === "password")?.enabled ?? false,
  );

  async function revoke(device: { id: number; current: boolean }) {
    problem = null;
    if (device.current) {
      // Signing this browser out also clears its cookie, which only the service can do.
      await session.signOut();
      return;
    }
    const result = await store.revoke(device.id);
    if (!result.ok) problem = result.message;
  }

  async function setPassword(enabled: boolean) {
    problem = null;
    const result = await store.setMethod("password", enabled);
    if (!result.ok) problem = result.message;
  }

  async function change(event: SubmitEvent) {
    event.preventDefault();
    passwordProblem = null;
    changed = false;
    if (next.length < MIN_PASSWORD_CHARS) {
      passwordProblem = `A password needs at least ${MIN_PASSWORD_CHARS} characters.`;
      return;
    }
    if (next !== again) {
      passwordProblem = "The two passwords are not the same.";
      return;
    }
    const result = await store.changePassword(current, next);
    current = "";
    next = "";
    again = "";
    if (result.ok) changed = true;
    else passwordProblem = result.message;
  }
</script>

<h3 id="{id}-title">Devices</h3>
{#if store.status === "failed"}
  <div role="status">
    <p class="muted">Couldn't read the devices.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else if store.status === "ready"}
  <p class="desc">
    Browsers signed in as you. Signing one out ends its session, its live
    updates and its terminals at once.
  </p>
  {#if store.devices.length > 0}
    <ul class="list" aria-label="Signed-in devices">
      {#each store.devices as device (device.id)}
        <li>
          <span class="name">{device.name}</span>
          {#if device.current}<span class="tag">This browser</span>{/if}
          <span class="muted" title={absoluteTime(device.last_seen_at)}
            >last used {relativeTime(device.last_seen_at, Date.now())}</span
          >
          <button
            type="button"
            class="btn"
            aria-label="Sign out {device.name}{device.current
              ? ' (this browser)'
              : ''}"
            onclick={() => revoke(device)}>Sign out</button
          >
        </li>
      {/each}
    </ul>
  {:else}
    <p class="muted">No browser is signed in.</p>
  {/if}
  <p class="error" role="alert" aria-live="polite">{problem ?? ""}</p>

  {#if session.mode === "signed-in"}
    <h3 id="{id}-password-title">Password</h3>
    <form onsubmit={change} aria-labelledby="{id}-password-title">
      <div class="fields">
        <label for="{id}-current">Current password</label>
        <input
          id="{id}-current"
          type="password"
          autocomplete="current-password"
          bind:value={current}
          oninput={() => (changed = false)}
          required
        />
        <label for="{id}-new">New password</label>
        <input
          id="{id}-new"
          type="password"
          autocomplete="new-password"
          bind:value={next}
          oninput={() => (changed = false)}
          required
        />
        <label for="{id}-again">The new password again</label>
        <input
          id="{id}-again"
          type="password"
          autocomplete="new-password"
          bind:value={again}
          oninput={() => (changed = false)}
          required
        />
      </div>
      <div class="row">
        <button type="submit" class="btn primary">Change password</button>
        <span aria-live="polite">
          {#if changed}<span class="saved"
              >Changed. Your other browsers are signed out.</span
            >{/if}
          {#if passwordProblem}<span class="error" role="alert"
              >{passwordProblem}</span
            >{/if}
        </span>
      </div>
    </form>
  {/if}

  {#if store.methods.length > 0}
    <h3>Ways to sign in</h3>
    <label class="check">
      <input
        type="checkbox"
        checked={passwordOn}
        onchange={(event) => setPassword(event.currentTarget.checked)}
      />
      Sign in with a password
    </label>
    <p class="muted">
      Browsers already signed in stay signed in. At least one way of signing in
      stays on.
    </p>
  {/if}
{/if}

<style>
  .list {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .list li,
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
  }
  .name {
    font-weight: 600;
  }
  .tag {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
  }
  form,
  .fields {
    display: grid;
    gap: var(--space-2);
  }
  .check {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .error {
    color: var(--color-danger);
    margin: 0;
  }
  .muted {
    color: var(--color-text-muted);
  }
</style>
