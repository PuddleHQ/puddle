<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "#lib/api/client.ts";
  import { KIND_LABEL, errorText, type UiError } from "#lib/errors/listing.ts";
  import { absoluteTime, relativeTime } from "#lib/format/relative-time.ts";
  import "#lib/theme/controls.css";

  let status = $state<"loading" | "failed" | "ready">("loading");
  let errors = $state<UiError[]>([]);
  let dropped = $state(0);
  let now = $state(Date.now());
  let heading = $state<HTMLElement>();
  let copied = $state<{ at: number; ok: boolean } | null>(null);

  async function load() {
    try {
      const { data } = await api.GET("/api/ui-errors");
      if (!data) throw new Error("no list");
      errors = data.errors;
      dropped = data.dropped;
      now = Date.now();
      status = "ready";
    } catch {
      status = "failed";
    }
  }

  async function copy(error: UiError) {
    try {
      await navigator.clipboard.writeText(errorText(error));
      copied = { at: error.at, ok: true };
    } catch {
      copied = { at: error.at, ok: false };
    }
  }

  onMount(() => {
    void load();
    heading?.focus();
  });
</script>

<div class="head">
  <div>
    <p class="crumb"><a href="/settings/this-computer">Settings</a> /</p>
    <h1 bind:this={heading} tabindex="-1">App errors</h1>
    <p class="sub">
      Errors Puddle's pages hit that no code expected, since Puddle started.
      Each one is also in Puddle's log, with addresses and tokens taken out.
    </p>
  </div>
  <button type="button" class="btn" onclick={() => void load()}>Refresh</button>
</div>

{#if status === "loading"}
  <p class="muted" role="status">Loading&hellip;</p>
{:else if status === "failed"}
  <p class="error" role="alert">
    Couldn't read the errors. Puddle's service isn't answering.
  </p>
{:else}
  {#if dropped > 0}
    <p class="muted">
      {dropped === 1 ? "1 more error was" : `${dropped} more errors were`} left out
      because pages sent too many at once.
    </p>
  {/if}
  {#if errors.length === 0}
    <p class="ok" role="status">No errors since Puddle started.</p>
  {:else}
    <ol class="list" aria-label="App errors">
      {#each errors as error, i (`${error.at}-${i}`)}
        <li class="card">
          <p class="meta">
            <b>{KIND_LABEL[error.kind]}</b>
            <span title={absoluteTime(error.at)}
              >{relativeTime(error.at, now)}</span
            >
            {#if error.route}<span>on <code>{error.route}</code></span>{/if}
            {#if error.build}<span class="muted">build {error.build}</span>{/if}
          </p>
          <pre class="message">{error.message}</pre>
          {#if error.stack}
            <details>
              <summary>Stack</summary>
              <pre>{error.stack}</pre>
            </details>
          {/if}
          <div class="actions">
            <button type="button" class="btn" onclick={() => copy(error)}
              >Copy<span class="visually-hidden">
                the error from {absoluteTime(error.at)}</span
              ></button
            >
            <span role="status"
              >{copied?.at === error.at
                ? copied.ok
                  ? "Copied."
                  : "Couldn't copy it. Select the text and copy it by hand."
                : ""}</span
            >
          </div>
        </li>
      {/each}
    </ol>
  {/if}
{/if}

<style>
  .head {
    display: flex;
    justify-content: space-between;
    align-items: start;
    gap: var(--space-4);
  }
  h1 {
    margin: 0;
  }
  h1:focus {
    outline: none;
  }
  .crumb {
    margin: 0 0 var(--space-2);
  }
  .sub,
  .muted {
    color: var(--color-text-muted);
  }
  .sub {
    margin: var(--space-2) 0 var(--space-4);
    max-width: 44rem;
  }
  p {
    margin: 0;
  }
  .error {
    color: var(--color-danger);
  }
  .ok {
    color: var(--color-success);
  }
  .list {
    display: grid;
    gap: var(--space-3);
    max-width: 60rem;
    margin: var(--space-3) 0 0;
    padding: 0;
    list-style: none;
  }
  .card {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  pre {
    margin: 0;
    overflow-x: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
</style>
