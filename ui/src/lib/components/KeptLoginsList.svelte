<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { absoluteTime } from "#lib/format/relative-time.ts";
  import { forgetConfirm, endKeptLoginNotices } from "#lib/notify/logins.ts";
  import { notices } from "#lib/notify/notices.svelte.ts";
  import type {
    KeptLogin,
    KeptLogins,
  } from "#lib/stores/kept-logins.svelte.ts";
  import "#lib/theme/controls.css";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  // The logins Puddle keeps for one workspace, each with the way to forget it. It is the way in
  // after a notice was dismissed; the list holds names and times, never a token.
  let {
    store,
    workspaceName,
    workspaceLabel,
    reload,
  }: {
    store: KeptLogins;
    /** The name the notices are keyed by. */
    workspaceName: string;
    /** What the confirmation calls the workspace. */
    workspaceLabel: string;
    reload: () => void;
  } = $props();

  const headingId = $props.id();

  let asking = $state<KeptLogin | null>(null);
  let askOpen = $state(false);
  let working = $state(false);
  let failure = $state<string | null>(null);
  /** The sentence the live region says after a login was forgotten. */
  let done = $state<string | null>(null);

  function ask(login: KeptLogin) {
    failure = null;
    done = null;
    asking = login;
    askOpen = true;
  }

  async function confirmed() {
    if (!asking) return;
    const login = asking;
    working = true;
    const result = await store.forget(login.service);
    working = false;
    if (!result.ok) {
      failure = result.message;
      return;
    }
    endKeptLoginNotices(notices, workspaceName, login.name);
    done = `Forgot the ${login.name} login.`;
    askOpen = false;
  }

  /** After a forget the row and its button are gone: focus goes to the heading, not the page. */
  const afterClose = () =>
    done === null ? undefined : document.getElementById(headingId);
</script>

<section class="kept" aria-labelledby={headingId}>
  <h3 id={headingId} tabindex="-1">Kept logins</h3>
  {#if store.status === "loading"}
    <p class="desc">Loading the kept logins&hellip;</p>
  {:else if store.status === "failed"}
    <div role="alert">
      <p class="desc">Couldn't read the kept logins.</p>
      {#if store.reason}<p class="error">{store.reason}</p>{/if}
    </div>
    <button type="button" class="btn" onclick={reload}>Try again</button>
  {:else if !store.storeReadable}
    <div role="alert">
      <p class="desc">
        Puddle couldn't read this computer's credential store, so it can't say
        which logins it keeps for this workspace. Unlock the store and try
        again.
      </p>
    </div>
    <button type="button" class="btn" onclick={reload}>Try again</button>
  {:else if store.logins.length === 0}
    <p class="desc">
      Puddle keeps no logins for this workspace. A login made inside it appears
      here while capture is on.
    </p>
  {:else}
    <ul>
      {#each store.logins as login (login.service)}
        <li>
          <div class="grow">
            <span class="name">{login.name}</span>
            <span class="desc">
              {login.since === null
                ? "Kept since a time Puddle did not record."
                : `Kept since ${absoluteTime(login.since)}.`}
            </span>
          </div>
          <button
            type="button"
            class="btn"
            aria-label="Forget the {login.name} login"
            onclick={() => ask(login)}>Forget</button
          >
        </li>
      {/each}
    </ul>
  {/if}
  <p class="desc" role="status">{done ?? ""}</p>
</section>

{#if asking}
  {@const confirm = forgetConfirm(workspaceLabel, asking.name)}
  <ConfirmDialog
    bind:open={askOpen}
    title={confirm.title}
    summary={confirm.summary}
    detail={confirm.detail}
    confirmLabel={confirm.confirmLabel}
    tone="deny"
    holdOpen
    closeFocus={afterClose}
    {working}
    error={failure}
    onConfirm={() => void confirmed()}
  />
{/if}

<style>
  .kept {
    display: grid;
    gap: var(--space-2);
    padding-top: var(--space-3);
    border-top: 1px solid var(--color-border-subtle);
  }
  h3 {
    margin: 0;
    font-size: var(--text-md);
  }
  ul {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }
  .grow {
    display: grid;
    flex: 1;
    min-width: 0;
  }
  .name {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .desc {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .error {
    margin: 0;
    color: var(--color-danger);
  }
</style>
