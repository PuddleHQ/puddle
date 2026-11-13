<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { goto } from "$app/navigation";
  import DuckMark from "./DuckMark.svelte";
  import {
    MIN_PASSWORD_CHARS,
    type SessionStore,
  } from "#lib/session/session.svelte.ts";
  import { APP_NAME } from "#lib/nav.ts";
  import "#lib/theme/controls.css";

  // What a browser sees under a public name before it is signed in: the first visitor makes the
  // owner account, everyone after signs in with a name and a password, and someone with an invite
  // link from the server sets a password. The desktop window never shows this.
  let { session }: { session: SessionStore } = $props();

  const id = $props.id();
  let name = $state("");
  let password = $state("");
  let again = $state("");
  let problem = $state<string | null>(null);

  const invited = $derived(session.invite !== null);
  const making = $derived(invited || session.mode === "setup");
  const heading = $derived(
    invited
      ? "Set a new password"
      : session.mode === "setup"
        ? `Welcome to ${APP_NAME}`
        : `Sign in to ${APP_NAME}`,
  );

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    problem = null;
    if (making) {
      if (password.length < MIN_PASSWORD_CHARS) {
        problem = `A password needs at least ${MIN_PASSWORD_CHARS} characters.`;
        return;
      }
      if (password !== again) {
        problem = "The two passwords are not the same.";
        return;
      }
    }
    const outcome = invited
      ? await session.redeemInvite(session.invite ?? "", password)
      : session.mode === "setup"
        ? await session.setup(name.trim(), password)
        : await session.signIn(name.trim(), password);
    // Nothing keeps a password once it has been sent.
    password = "";
    again = "";
    if (!outcome.ok) problem = outcome.message;
    // The invite link's own address is done with; carry on at the app's start.
    else if (invited) await goto("/");
  }
</script>

<main class="gate">
  <form onsubmit={submit} aria-labelledby="{id}-title">
    <p class="brand"><DuckMark size={40} /></p>
    <h1 id="{id}-title">{heading}</h1>
    {#if session.mode === "unavailable"}
      <p class="error" role="alert">
        Puddle's service isn't answering. Reload this page to try again.
      </p>
    {:else if !session.passwordLogin && !invited}
      <p role="alert" class="error">
        Signing in with a password is switched off here. Use another way of
        signing in.
      </p>
    {:else}
      {#if session.mode === "setup" && !invited}
        <p class="desc">
          Nobody has set this Puddle up yet. The account you make now is the
          owner's: it can see and change everything here.
        </p>
      {/if}
      {#if !invited}
        <label for="{id}-name">User name</label>
        <input
          id="{id}-name"
          type="text"
          autocomplete="username"
          autocapitalize="none"
          spellcheck="false"
          bind:value={name}
          required
        />
      {/if}
      <label for="{id}-password">{invited ? "New password" : "Password"}</label>
      <input
        id="{id}-password"
        type="password"
        autocomplete={making ? "new-password" : "current-password"}
        bind:value={password}
        required
      />
      {#if making}
        <p class="muted">At least {MIN_PASSWORD_CHARS} characters.</p>
        <label for="{id}-again">The same password again</label>
        <input
          id="{id}-again"
          type="password"
          autocomplete="new-password"
          bind:value={again}
          required
        />
      {/if}
      <p class="error" role="alert" aria-live="polite">{problem ?? ""}</p>
      <button type="submit" class="btn primary" disabled={session.busy}>
        {invited
          ? "Set password and sign in"
          : session.mode === "setup"
            ? "Create the owner account"
            : "Sign in"}
      </button>
    {/if}
  </form>
</main>

<style>
  .gate {
    display: grid;
    place-items: center;
    min-height: 100dvh;
    padding: var(--space-4);
  }
  form {
    display: grid;
    gap: var(--space-2);
    width: min(24rem, 100%);
    padding: var(--space-6);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .brand {
    margin: 0;
    justify-self: center;
  }
  h1 {
    margin: 0 0 var(--space-2);
    font-size: var(--text-lg);
    text-align: center;
  }
  .desc,
  .muted {
    margin: 0;
    color: var(--color-text-muted);
  }
  .error {
    margin: 0;
    min-height: 1.5em;
    color: var(--color-danger);
  }
  button {
    justify-self: stretch;
  }
</style>
