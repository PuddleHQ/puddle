<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import "#lib/theme/fonts.css";
  import "#lib/theme/tokens.css";
  import "#lib/theme/base.css";
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import Notices from "#lib/components/Notices.svelte";
  import Sidebar from "#lib/components/Sidebar.svelte";
  import { documentTitle } from "#lib/nav.ts";
  import { NoticeWatcher } from "#lib/notify/watcher.ts";
  import { live } from "#lib/stores/live.svelte.ts";
  import { networkHealth } from "#lib/stores/network-health.svelte.ts";
  import { density } from "#lib/theme/density.svelte.ts";
  import { theme } from "#lib/theme/theme.svelte.ts";
  import { isWelcome } from "#lib/welcome/steps.ts";

  let { children } = $props();

  const watcher = new NoticeWatcher({ source: live });
  // The first-run flow has the whole window: the app's sidebar would invite leaving it half done.
  const welcome = $derived(isWelcome(page.url.pathname));

  onMount(() => {
    theme.init();
    void theme.sync();
    density.init();
    void density.sync();
    const stops = [live.start(), networkHealth.start(), watcher.start()];
    return () => stops.forEach((stop) => stop());
  });

  // A new report (the first read, or after `network_changed`) raises or withdraws the notice.
  $effect(() => {
    watcher.network(networkHealth.report);
  });
</script>

<svelte:head>
  <title>{documentTitle(page.url.pathname, live.pending)}</title>
</svelte:head>

<a class="skip-link" href="#main">Skip to content</a>
<div class="shell">
  {#if !welcome}<Sidebar />{/if}
  <div class="content">
    {#if live.problem === "unauthorized"}
      <p class="banner" role="status">
        Puddle can't sign in to its service. Open Puddle from its app window or
        tray icon.
      </p>
    {:else if live.problem === "unreachable"}
      <p class="banner" role="status">
        Puddle's service isn't answering. Retrying.
      </p>
    {:else if live.problem === "failing"}
      <p class="banner" role="status">
        {live.problemDetail} Retrying.
      </p>
    {:else if live.streamError}
      <p class="banner" role="status">
        Live updates paused: {live.streamError}. Puddle keeps trying and
        refreshes every few seconds meanwhile.
      </p>
    {/if}
    <Notices />
    <main id="main" tabindex="-1">
      {@render children()}
    </main>
  </div>
</div>

<style>
  .shell {
    display: flex;
    min-height: 100vh;
  }
  .content {
    flex: 1;
    min-width: 0;
  }
  main {
    padding: var(--space-6) var(--space-8);
  }
  main:focus {
    outline: none;
  }
  .banner {
    margin: 0;
    padding: var(--space-2) var(--space-8);
    background: var(--color-surface-raised);
    border-bottom: 1px solid var(--color-border-subtle);
    color: var(--color-text);
  }
  @media (max-width: 760px) {
    .shell {
      flex-direction: column;
    }
    main {
      padding: var(--space-4);
    }
    .banner {
      padding: var(--space-2) var(--space-4);
    }
  }
  .skip-link {
    position: absolute;
    inset-inline-start: var(--space-2);
    top: -3rem;
    padding: var(--space-2) var(--space-3);
    background: var(--color-surface);
    color: var(--color-text);
    border-radius: var(--radius-md);
    z-index: 10;
  }
  .skip-link:focus {
    top: var(--space-2);
  }
</style>
