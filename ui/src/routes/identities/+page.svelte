<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import ArrowDown from "@lucide/svelte/icons/arrow-down";
  import ArrowUp from "@lucide/svelte/icons/arrow-up";
  import GripVertical from "@lucide/svelte/icons/grip-vertical";
  import Plus from "@lucide/svelte/icons/plus";
  import CheckChip from "#lib/components/CheckChip.svelte";
  import ConfirmDialog from "#lib/components/ConfirmDialog.svelte";
  import IdentityDialog from "#lib/components/IdentityDialog.svelte";
  import Toast from "#lib/components/Toast.svelte";
  import {
    coverageText,
    sourceLabel,
    identityStatus,
    moved,
    movedTo,
    usedBy,
    type Identity,
  } from "#lib/identities/model.ts";
  import { identities as store } from "#lib/stores/identities.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import "#lib/theme/controls.css";

  let heading = $state<HTMLElement>();
  let addOpen = $state(false);
  let deleting = $state<Identity | null>(null);
  let deleteOpen = $state(false);
  let testing = $state(false);

  const ids = $derived(store.identities.map((i) => i.id));

  onMount(() => store.start());

  async function showError(message: string) {
    toasts.push(message, { tone: "error" });
    await tick();
  }

  async function makeDefault(identity: Identity) {
    const result = await store.makeDefault(identity.id);
    if (result.ok) toasts.push(`${identity.label} is now the default.`);
    else await showError(result.message);
  }

  let dragging = $state<number | null>(null);

  async function dropOn(target: Identity) {
    const from = dragging;
    dragging = null;
    if (from === null || from === target.id) return;
    const result = await store.reorder(movedTo(ids, from, target.id));
    if (!result.ok) await showError(result.message);
  }

  async function move(identity: Identity, by: -1 | 1) {
    const result = await store.reorder(moved(ids, identity.id, by));
    if (!result.ok) await showError(result.message);
    await tick();
    // A button that became disabled (the identity reached an end) hands focus to its sibling.
    const active = document.activeElement;
    if (
      !active ||
      active === document.body ||
      (active instanceof HTMLButtonElement && active.disabled)
    ) {
      document
        .querySelector(`[data-identity-id="${identity.id}"]`)
        ?.querySelector<HTMLElement>("[data-move]:not(:disabled)")
        ?.focus();
    }
  }

  async function testAll() {
    testing = true;
    try {
      await store.checkAll();
    } finally {
      testing = false;
    }
  }

  function askDelete(identity: Identity) {
    deleting = identity;
    deleteOpen = true;
  }

  async function confirmDelete() {
    const identity = deleting;
    if (!identity) return;
    const result = await store.remove(identity.id);
    if (!result.ok) return showError(result.message);
    const left = result.value;
    toasts.push(
      left.length === 0
        ? `Deleted ${identity.label}.`
        : `Deleted ${identity.label}; it was taken off ${left.join(", ")}.`,
    );
    await focusHeading();
  }

  // The row the dialog was opened from is gone, so focus goes to the heading once the dialog has
  // finished giving it back.
  async function focusHeading() {
    for (let frame = 0; frame < 30; frame += 1) {
      await tick();
      if (!document.querySelector('[role="alertdialog"], [role="dialog"]'))
        break;
      await new Promise((resolve) => requestAnimationFrame(resolve));
    }
    await tick();
    heading?.focus();
  }

  function deleteDetail(identity: Identity): string {
    const names = identity.workspaces;
    return names.length === 0
      ? "No workspace uses it. Pasted tokens it holds are removed from your operating system's credential store."
      : `${names.join(", ")} will lose it and go without its credentials and author. Pasted tokens it holds are removed from your operating system's credential store.`;
  }
</script>

<div class="head">
  <div>
    <h1 tabindex="-1" bind:this={heading}>Identities</h1>
    <p class="sub">
      Who commits and signs in: a commit author plus the credentials Puddle adds
      to Git requests. The secrets never enter a workspace.
    </p>
  </div>
  <div class="acts">
    {#if store.identities.length > 0}
      <button type="button" class="btn" disabled={testing} onclick={testAll}>
        {testing ? "Testing…" : "Test all"}
      </button>
    {/if}
    <button type="button" class="btn primary" onclick={() => (addOpen = true)}>
      <Plus aria-hidden="true" size={16} />Add identity
    </button>
  </div>
</div>

{#if store.status === "loading"}
  <p class="muted">Loading identities&hellip;</p>
{:else if store.status === "failed"}
  <p class="muted">Couldn't read the identities yet. Trying again.</p>
{:else if store.identities.length === 0}
  <section class="empty">
    <h2>No identities yet</h2>
    <p>
      Add one to commit under your name in a workspace and to let it clone and
      push private repositories. Without one, a workspace can still read public
      repositories.
    </p>
  </section>
{:else}
  <!-- Seven columns do not fit a phone: the table scrolls inside its own frame, which a key reaches. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="wrap" role="region" aria-label="Identities table" tabindex="0">
    <table class="rows">
      <caption class="visually-hidden">Identities, in your order</caption>
      <thead>
        <tr>
          <th scope="col">Name</th>
          <th scope="col">Commit author</th>
          <th scope="col">Covers</th>
          <th scope="col">Last test</th>
          <th scope="col">Used by</th>
          <th scope="col">
            Order{" "}<span class="visually-hidden">
              (first match is the commit author)
            </span>
          </th>
          <th scope="col"><span class="visually-hidden">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each store.identities as identity, index (identity.id)}
          {@const status = identityStatus(
            identity.credentials.map((c) => store.checkOf(c)),
          )}
          <tr
            class="row"
            class:over={dragging !== null && dragging !== identity.id}
            data-identity-id={identity.id}
            draggable="true"
            ondragstart={() => (dragging = identity.id)}
            ondragend={() => (dragging = null)}
            ondragover={(e) => dragging !== null && e.preventDefault()}
            ondrop={(e) => {
              e.preventDefault();
              void dropOn(identity);
            }}
          >
            <th scope="row">
              <a href="/identities/{identity.id}">{identity.label}</a>
              {#if identity.is_default}<span class="meta">Default</span>{/if}
            </th>
            <td>{identity.author.name} &lt;{identity.author.email}&gt;</td>
            <td>
              {#if identity.credentials.length === 0}
                <span class="muted">No credentials: commits only.</span>
              {:else}
                <ul class="covers" aria-label="Credentials of {identity.label}">
                  {#each identity.credentials as credential, at (at)}
                    <li>
                      {credential.host}: {coverageText(
                        credential.covers,
                        credential.host,
                      )}
                      <span class="muted">
                        ({sourceLabel(credential.source)})
                      </span>
                    </li>
                  {/each}
                </ul>
              {/if}
            </td>
            <td>
              {#if identity.credentials.length > 0}<CheckChip
                  check={status}
                />{:else}<span class="muted">-</span>{/if}
            </td>
            <td>{usedBy(identity)}</td>
            <td class="order">
              <GripVertical aria-hidden="true" size={16} class="grip" />
              <button
                type="button"
                class="btn"
                data-move="up"
                aria-label="Move {identity.label} up"
                disabled={index === 0}
                onclick={() => move(identity, -1)}
              >
                <ArrowUp aria-hidden="true" size={16} />
              </button>
              <button
                type="button"
                class="btn"
                data-move="down"
                aria-label="Move {identity.label} down"
                disabled={index === store.identities.length - 1}
                onclick={() => move(identity, 1)}
              >
                <ArrowDown aria-hidden="true" size={16} />
              </button>
            </td>
            <td class="acts">
              {#if !identity.is_default}
                <button
                  type="button"
                  class="btn"
                  onclick={() => makeDefault(identity)}
                >
                  Set {identity.label} as default
                </button>
              {/if}
              <button
                type="button"
                class="btn deny"
                aria-label="Delete {identity.label}"
                onclick={() => askDelete(identity)}
              >
                Delete
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<p class="muted settings-link">
  Whether new workspaces may push to or pull from any repository is set in <a
    href="/settings/editor-and-git"
  >
    Settings, Git
  </a>.
</p>

<IdentityDialog
  bind:open={addOpen}
  mode="create"
  {store}
  onSaved={(made) => toasts.push(`Added ${made.label}.`)}
/>

{#if deleting}
  <ConfirmDialog
    bind:open={deleteOpen}
    title="Delete identity"
    summary="Delete {deleting.label}?"
    detail={deleteDetail(deleting)}
    confirmLabel="Delete {deleting.label}"
    tone="deny"
    onConfirm={confirmDelete}
  />
{/if}
<Toast />

<style>
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    flex-wrap: wrap;
    gap: var(--space-4);
    margin-bottom: var(--space-4);
  }
  h2 {
    font-size: var(--text-lg);
  }
  .sub,
  .muted {
    margin: var(--space-1) 0 0;
    color: var(--color-text-muted);
  }
  .sub {
    max-width: var(--page-reading-width);
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    gap: var(--space-2);
  }
  /* Positioned so a visually-hidden label inside a cell is clipped with the table, not the page. */
  .wrap {
    position: relative;
    overflow-x: auto;
  }
  .rows {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  .rows th,
  .rows td {
    padding: var(--space-3);
    text-align: start;
    vertical-align: top;
    overflow-wrap: anywhere;
    border-bottom: 1px solid var(--color-border-subtle);
  }
  .rows thead th {
    color: var(--color-text-muted);
    font-weight: 600;
  }
  .rows tbody th {
    font-weight: 600;
  }
  .row.over {
    cursor: grabbing;
  }
  .covers {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .order {
    white-space: nowrap;
  }
  .order :global(.grip) {
    vertical-align: middle;
    color: var(--color-text-muted);
    cursor: grab;
  }
  .settings-link {
    margin-top: var(--space-4);
  }
  /* An empty screen is plain words on the page, not a box. */
  .empty {
    padding-block: var(--space-4);
  }
  .empty p {
    margin: var(--space-2) 0 0;
    color: var(--color-text-muted);
  }
</style>
