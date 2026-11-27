<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { page } from "$app/state";
  import {
    SETTINGS_SECTIONS,
    activeSection,
    tabHref,
  } from "#lib/workspaces/tabs.ts";

  let { children } = $props();

  const id = $derived(page.params["id"] ?? "");
  const current = $derived(activeSection(page.url.pathname, id));
</script>

<div class="settings">
  <nav aria-label="Workspace settings">
    <ul>
      {#each SETTINGS_SECTIONS as section (section.slug)}
        <li>
          <a
            href={section.slug === ""
              ? tabHref(id, "settings")
              : tabHref(id, section.slug)}
            aria-current={current === section.slug ? "page" : undefined}
          >
            {section.label}
          </a>
        </li>
      {/each}
    </ul>
  </nav>
  <div class="body">{@render children()}</div>
</div>

<style>
  .settings {
    display: grid;
    grid-template-columns: 11rem minmax(0, 1fr);
    gap: var(--space-6);
    align-items: start;
  }
  ul {
    display: grid;
    gap: var(--space-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  a {
    display: block;
    padding: var(--space-2) var(--space-3);
    border-inline-start: 3px solid transparent;
    color: var(--color-text);
    text-decoration: none;
  }
  a:hover {
    background: var(--color-surface-raised);
  }
  a[aria-current="page"] {
    border-inline-start-color: var(--color-accent);
    font-weight: 600;
  }
  .body {
    min-width: 0;
  }
  @media (max-width: 760px) {
    .settings {
      grid-template-columns: minmax(0, 1fr);
    }
    ul {
      display: flex;
      flex-wrap: wrap;
    }
    a {
      border-inline-start: 0;
      border-bottom: 3px solid transparent;
    }
    a[aria-current="page"] {
      border-bottom-color: var(--color-accent);
    }
  }
</style>
