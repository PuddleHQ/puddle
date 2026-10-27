<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { page } from "$app/state";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import { ShellInitStore } from "#lib/stores/workspace-shell-init.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import "#lib/theme/controls.css";

  const id = $derived(page.params["id"] ?? "");
  const workspace = $derived(workspaces.list.find((w) => w.id === id));
  const name = $derived(workspace?.name);
  const store = new ShellInitStore();
  const fid = $props.id();

  // What the user typed, with the save it started from. `null`: the box follows the saved text.
  let edit = $state<{ text: string; base: number | null } | null>(null);
  let problem = $state<string | null>(null);
  let outcome = $state<string | null>(null);
  let busy = $state(false);

  $effect(() => {
    if (name) return store.start(name);
  });

  const view = $derived(store.view);
  const text = $derived(edit?.text ?? view?.content ?? "");
  const dirty = $derived(edit !== null && edit.text !== (view?.content ?? ""));
  // The saved script changed (another window, a restore) while the user was typing.
  const stale = $derived(
    edit !== null && view !== null && edit.base !== view.changed_at,
  );

  function type(next: string) {
    edit = { text: next, base: edit?.base ?? view?.changed_at ?? null };
    problem = null;
    outcome = null;
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (busy) return;
    busy = true;
    problem = null;
    outcome = null;
    const result = await store.save(text);
    busy = false;
    if (!result.ok) {
      problem = result.message;
      return;
    }
    edit = null;
    outcome = result.value.checked
      ? "Saved. Puddle ran it once in the workspace and it printed nothing."
      : `Saved. ${result.value.unchecked_reason ?? "The workspace is not running, so Puddle checks the script when it starts."}`;
    toasts.push("Saved the shell init.");
  }

  async function remove() {
    if (busy) return;
    busy = true;
    problem = null;
    outcome = null;
    const kept = text;
    const result = await store.remove();
    busy = false;
    if (!result.ok) {
      problem = result.message;
      return;
    }
    // The text stays in the box, so Save puts it back.
    edit = { text: kept, base: store.view?.changed_at ?? null };
    toasts.push("Removed the shell init. Its text is still in the box.");
  }
</script>

{#if workspace && view}
  <section aria-labelledby="{fid}-h">
    <h2 id="{fid}-h">Shell init</h2>
    <p class="muted">
      A script that every login shell in this workspace runs: bash and zsh
      terminals and SSH logins. It must print nothing, because tools that script
      a login shell read its output. Puddle keeps the text on this computer and
      the workspace sees it read-only; a change made from inside is undone at
      the next start.
    </p>

    {#if store.restored}
      <div class="warn" role="status">
        <p>
          Something in the workspace changed Puddle's copy of this file. Puddle
          put your saved text back.
        </p>
        <button
          type="button"
          class="btn"
          onclick={() => (store.restored = false)}>Dismiss</button
        >
      </div>
    {/if}
    {#if stale}
      <div class="warn" role="status">
        <p>
          The saved script changed while you were editing. Your edits are kept
          until you load it.
        </p>
        <button type="button" class="btn" onclick={() => (edit = null)}
          >Load the saved script</button
        >
      </div>
    {/if}

    <form onsubmit={save} novalidate>
      <div class="field">
        <label for="{fid}-text">Script for {workspace.name}</label>
        <textarea
          id="{fid}-text"
          class="mono"
          rows="14"
          spellcheck="false"
          autocapitalize="off"
          placeholder="# export EDITOR=vim"
          value={text}
          oninput={(e) => type(e.currentTarget.value)}
          aria-describedby={problem ? `${fid}-error` : `${fid}-hint`}
          aria-invalid={problem ? "true" : undefined}></textarea>
        <p class="hint" id="{fid}-hint">
          {view.saved ? "Saved." : "Nothing saved yet."}
          Up to 48 KiB. Line ends are saved as LF.
        </p>
      </div>
      {#if problem}
        <p class="error" id="{fid}-error" role="alert">
          Not saved. {problem}
        </p>
      {/if}
      {#if outcome}
        <p class="ok" role="status">{outcome}</p>
      {/if}
      <div class="acts">
        <button
          type="submit"
          class="btn primary"
          disabled={busy || !dirty || text === ""}>Save</button
        >
        <button
          type="button"
          class="btn"
          disabled={busy || !dirty}
          onclick={() => {
            edit = null;
            problem = null;
          }}>Discard changes</button
        >
        <button
          type="button"
          class="btn deny"
          disabled={busy || !view.saved}
          onclick={remove}>Remove the script</button
        >
      </div>
    </form>
  </section>
{:else if store.status === "failed"}
  <p class="muted">Couldn't read the shell init yet. Trying again.</p>
{:else}
  <p class="muted">Loading shell init&hellip;</p>
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
  .muted,
  .hint {
    color: var(--color-text-muted);
  }
  .hint {
    font-size: var(--text-sm);
  }
  .error {
    color: var(--color-danger);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .ok {
    color: var(--color-text);
  }
  .warn {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
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
  textarea {
    box-sizing: border-box;
    width: 100%;
    padding: var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font-size: var(--text-sm);
    resize: vertical;
  }
  textarea[aria-invalid="true"] {
    border-color: var(--color-danger);
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
</style>
