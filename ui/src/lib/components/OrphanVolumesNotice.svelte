<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { tick } from "svelte";
  import type {
    OrphanVolume,
    OrphanVolumes,
  } from "#lib/stores/orphan-volumes.svelte.ts";
  import "#lib/theme/controls.css";

  // The workspace disks Puddle keeps whose workspace its list does not name. Puddle cannot tell
  // from outside whether such a disk holds any files, so this says only that it kept them. Each
  // one can be restored as a workspace; nothing here deletes a disk.
  let { store }: { store: OrphanVolumes } = $props();

  const headingId = $props.id();

  /** The disk being restored, and the disk that asked for an address. */
  let working = $state<string | null>(null);
  let asking = $state<string | null>(null);
  let address = $state("");
  let failure = $state<{ id: string; message: string } | null>(null);
  /** What the live region says after a restore. */
  let done = $state<string | null>(null);

  async function restore(volume: OrphanVolume) {
    working = volume.id;
    failure = null;
    done = null;
    const result = await store.restore(
      volume.id,
      asking === volume.id ? address : undefined,
    );
    working = null;
    if (result.ok) {
      asking = null;
      address = "";
      done = `Restored ${result.name} as a workspace. It is stopped; start it to use it.`;
      // The row and its button are gone: focus goes to the notice, not to the top of the page.
      await tick();
      document.getElementById(headingId)?.focus();
      return;
    }
    failure = { id: volume.id, message: result.message };
    if (result.needsAddress) asking = volume.id;
  }
</script>

{#if store.volumes.length > 0}
  <section class="notice" aria-labelledby={headingId}>
    <h2 id={headingId} tabindex="-1">
      {store.volumes.length === 1
        ? "A workspace disk is not in your workspace list"
        : `${store.volumes.length} workspace disks are not in your workspace list`}
    </h2>
    <p>
      Puddle kept {store.volumes.length === 1 ? "it" : "them"} and changed nothing.
      Puddle cannot tell from outside whether
      {store.volumes.length === 1 ? "it holds" : "they hold"} any files. This happens
      when the workspace list is lost or comes back from an old copy. Restoring a
      disk adds a workspace for it and changes nothing on the disk.
    </p>
    <ul>
      {#each store.volumes as volume (volume.id)}
        <li>
          <div class="row">
            <code>{volume.volume}</code>
            <button
              type="button"
              class="btn"
              aria-label="Restore as workspace {volume.volume}"
              disabled={working !== null}
              onclick={() => restore(volume)}
            >
              {working === volume.id ? "Restoring…" : "Restore as workspace"}
            </button>
          </div>
          {#if asking === volume.id}
            <label class="address">
              Repository address for {volume.id}
              <input
                type="url"
                bind:value={address}
                placeholder="https://github.com/owner/repository"
                autocomplete="off"
                spellcheck="false"
              />
            </label>
          {/if}
          {#if failure?.id === volume.id}
            <p class="failure" role="alert">{failure.message}</p>
          {/if}
        </li>
      {/each}
    </ul>
  </section>
{/if}
<p class="visually-hidden" role="status">{done ?? ""}</p>

<style>
  .notice {
    display: grid;
    gap: var(--space-3);
    margin-bottom: var(--space-4);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  h2,
  p {
    margin: 0;
  }
  h2 {
    font-size: var(--text-lg);
  }
  h2:focus {
    outline: none;
  }
  ul {
    display: grid;
    gap: var(--space-3);
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  .address {
    display: grid;
    gap: var(--space-1);
    margin-top: var(--space-2);
  }
  .failure {
    margin-top: var(--space-2);
    color: var(--color-text-muted);
  }
</style>
