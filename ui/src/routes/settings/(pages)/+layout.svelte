<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // The frame of Settings: a list of pages on the left with a search above it, and the page beside
  // it. Every page keeps its own short list of settings; each setting saves by itself (see
  // SettingRow), so there is no Save bar here.
  import { onMount, type Snippet } from "svelte";
  import { page } from "$app/state";
  import Toast from "#lib/components/Toast.svelte";
  import UnknownFields from "#lib/components/UnknownFields.svelte";
  import { provideSaver } from "#lib/settings/context.ts";
  import {
    PAGES,
    pageLabel,
    searchSettings,
    settingHref,
  } from "#lib/settings/pages.ts";
  import { SettingsSaver } from "#lib/settings/saver.svelte.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";
  import "#lib/theme/controls.css";

  let { children }: { children: Snippet } = $props();

  provideSaver(new SettingsSaver());
  onMount(() => void store.load(store.view !== null));

  let query = $state("");
  const results = $derived(searchSettings(query));
  const searching = $derived(query.trim() !== "");
  const here = $derived(page.url.pathname.replace(/\/$/, ""));
</script>

<h1 tabindex="-1">Settings</h1>
<p class="sub">
  Global values. A workspace can override most of them in its own Settings tab.
</p>

{#if store.status === "loading" && !store.view}
  <p class="muted">Loading settings&hellip;</p>
{:else if store.status === "newer"}
  <p class="error" role="alert">
    These settings were saved by a newer Puddle. Update Puddle to change them.
  </p>
{:else if !store.view || !store.consents}
  <div role="alert">
    <p class="muted">Couldn't read Puddle's settings.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else}
  <UnknownFields fields={store.view.unknown_fields} />
  <div class="layout">
    <div class="side">
      <input
        type="search"
        aria-label="Search settings"
        placeholder="Search settings"
        autocomplete="off"
        bind:value={query}
      />
      {#if searching}
        <section aria-label="Search results">
          {#if results.length === 0}
            <p class="muted">No setting matches "{query.trim()}".</p>
          {:else}
            <ul class="results">
              {#each results as hit (hit.id)}
                <li>
                  <a href={settingHref(hit)} onclick={() => (query = "")}>
                    {hit.label}<span class="where">{pageLabel(hit.page)}</span>
                  </a>
                </li>
              {/each}
            </ul>
          {/if}
        </section>
      {:else}
        <nav aria-label="Settings pages">
          {#each PAGES as entry (entry.id)}
            <a
              href={entry.href}
              aria-current={here === entry.href ? "page" : undefined}
            >
              {entry.label}
            </a>
          {/each}
        </nav>
      {/if}
    </div>
    <div class="content">
      {@render children()}
    </div>
  </div>
{/if}

<Toast />

<style>
  .sub,
  .muted {
    color: var(--color-text-muted);
  }
  .sub {
    margin: 0 0 var(--space-3);
  }
  .error {
    color: var(--color-danger);
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 14rem) minmax(0, 1fr);
    gap: var(--space-6);
    align-items: start;
  }
  .side {
    position: sticky;
    top: var(--space-4);
    display: grid;
    gap: var(--space-3);
  }
  nav,
  .results {
    display: grid;
    gap: var(--space-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  nav a,
  .results a {
    display: grid;
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-md);
    color: var(--color-text);
    text-decoration: none;
  }
  nav a:hover,
  .results a:hover {
    background: var(--color-surface-raised);
  }
  nav a[aria-current="page"] {
    background: var(--color-surface-raised);
    font-weight: 600;
  }
  .where {
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .content {
    max-width: var(--page-reading-width);
  }
  .content :global(:target) {
    outline: 2px solid var(--color-accent);
    outline-offset: 2px;
  }
  @media (max-width: 760px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
    .side {
      position: static;
    }
    nav {
      display: flex;
      flex-wrap: wrap;
    }
  }
</style>
