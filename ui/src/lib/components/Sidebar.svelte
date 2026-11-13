<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { page } from "$app/state";
  import { APP_NAME, NAV, sectionFor } from "#lib/nav.ts";
  import { session } from "#lib/session/session.svelte.ts";
  import { live } from "#lib/stores/live.svelte.ts";
  import DuckMark from "./DuckMark.svelte";
  import ThemeToggle from "./ThemeToggle.svelte";

  const current = $derived(sectionFor(page.url.pathname)?.href);
</script>

<aside class="sidebar">
  <p class="brand">
    <DuckMark size={28} />
    <span>{APP_NAME}</span>
  </p>
  <nav aria-label="Main">
    <ul>
      {#each NAV as item (item.href)}
        <li>
          <a
            href={item.href}
            aria-current={current === item.href ? "page" : undefined}
          >
            <item.icon aria-hidden="true" size={18} />
            <span>{item.label}</span>
            {#if item.badge === "pending" && live.pending > 0}
              <span class="badge" data-testid="pending-badge">
                <span aria-hidden="true">{live.pending}</span>
                <span class="visually-hidden">{live.pending} pending</span>
              </span>
            {/if}
          </a>
        </li>
      {/each}
    </ul>
  </nav>
  <div class="footer">
    {#if session.mode === "signed-in" && session.user}
      <p class="who">
        <span class="visually-hidden">Signed in as</span>
        {session.user.name}
        <button
          type="button"
          class="signout"
          onclick={() => void session.signOut()}>Sign out</button
        >
      </p>
    {/if}
    <ThemeToggle />
  </div>
</aside>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    width: var(--sidebar-width);
    flex: none;
    padding: var(--space-4);
    background: var(--color-surface);
    border-inline-end: 1px solid var(--color-border-subtle);
  }
  .who {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    margin: 0;
    color: var(--color-text-muted);
  }
  .signout {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  a {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    color: var(--color-text);
    text-decoration: none;
  }
  a:hover {
    background: var(--color-surface-raised);
  }
  a[aria-current="page"] {
    background: var(--color-accent-subtle);
    font-weight: 600;
  }
  .badge {
    margin-inline-start: auto;
    min-width: 1.5rem;
    padding: 0 var(--space-2);
    border-radius: var(--radius-pill);
    background: var(--color-accent);
    color: var(--color-accent-contrast);
    font-size: var(--text-sm);
    text-align: center;
  }
  .footer {
    margin-top: auto;
  }
  /* A phone-width window has no room for a column beside the page: the bar goes on top. */
  @media (max-width: 760px) {
    .sidebar {
      width: auto;
      flex-flow: row wrap;
      align-items: center;
      border-inline-end: none;
      border-block-end: 1px solid var(--color-border-subtle);
    }
    ul {
      flex-flow: row wrap;
    }
    .footer {
      margin-top: 0;
      margin-inline-start: auto;
    }
  }
</style>
