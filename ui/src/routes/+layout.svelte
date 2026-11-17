<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import "#lib/theme/fonts.css";
  import "#lib/theme/tokens.css";
  import "#lib/theme/base.css";
  import { onMount, untrack } from "svelte";
  import { goto } from "$app/navigation";
  import { version } from "$app/env";
  import { page } from "$app/state";
  import Notices from "#lib/components/Notices.svelte";
  import Sidebar from "#lib/components/Sidebar.svelte";
  import SignInGate from "#lib/components/SignInGate.svelte";
  import { documentTitle } from "#lib/nav.ts";
  import { api } from "#lib/api/client.ts";
  import { inDesktopApp } from "#lib/desktop/folder-picker.ts";
  import { NoticeWatcher } from "#lib/notify/watcher.ts";
  import { TabNotifier } from "#lib/notify/tab-notifier.ts";
  import { tabNotifications } from "#lib/notify/tab-switch.svelte.ts";
  import { clientId, keepClientId } from "#lib/presence/client-id.ts";
  import { PresenceReporter } from "#lib/presence/reporter.ts";
  import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import { session } from "#lib/session/session.svelte.ts";
  import { BuildGuard } from "#lib/stores/build-guard.ts";
  import { admin } from "#lib/stores/admin.svelte.ts";
  import { live } from "#lib/stores/live.svelte.ts";
  import { networkHealth } from "#lib/stores/network-health.svelte.ts";
  import { density } from "#lib/theme/density.svelte.ts";
  import { palette } from "#lib/theme/palette.svelte.ts";
  import { theme } from "#lib/theme/theme.svelte.ts";
  import { isWelcome } from "#lib/welcome/steps.ts";

  let { children } = $props();

  const watcher = new NoticeWatcher({ source: live });
  // Where a new request is announced: the service picks one client from what each reports, so a
  // person with several tabs, a phone and the desktop app is told once. The desktop window raises
  // its notification itself (the app), so only a browser tab shows a Web Notification.
  const desktop = inDesktopApp();
  const tabId = clientId();
  const reporter = new PresenceReporter({
    api,
    clientId: tabId,
    kind: desktop ? "desktop" : "browser",
    notify: () =>
      desktop
        ? (globalSettings.view?.ui.notifications ?? true)
        : tabNotifications.active,
  });
  const tabNotifier = desktop
    ? null
    : new TabNotifier({
        clientId: tabId,
        active: () => tabNotifications.active,
        labelOf: (id) => workspaces.labelOf(id),
        create: (title, options) => new Notification(title, options),
        open: (path) => {
          window.focus();
          void goto(path);
        },
      });
  // A tab left open across a server upgrade reloads into the build the server serves now.
  const build = new BuildGuard({ current: version, source: live });
  // The first-run flow has the whole window: the app's sidebar would invite leaving it half done.
  const welcome = $derived(isWelcome(page.url.pathname));

  // How this page is signed in: the desktop window has nothing to sign in to; a browser under a
  // public name signs in first.
  onMount(() => {
    void session.start();
    const stopMark = keepClientId(tabId);
    return () => {
      session.stop();
      stopMark();
    };
  });

  // The screens' own reads start once the page may make them, and stop when it is signed out.
  $effect(() => {
    if (!session.ready) return;
    // Only `ready` decides when they start and stop; what they read themselves is not a reason to
    // start them again.
    return untrack(() => {
      theme.init();
      void admin.probe();
      void theme.sync();
      density.init();
      void density.sync();
      palette.init();
      void palette.sync();
      if (desktop) void globalSettings.load(true);
      tabNotifications.refresh();
      const stops = [
        live.start(),
        networkHealth.start(),
        watcher.start(),
        build.start(),
        reporter.start(),
        live.subscribe({
          event: (e) => void tabNotifier?.handle(e),
          // The service may have restarted or the connection come back: look again for the screen.
          resync: () => void admin.probe(),
        }),
      ];
      return () => stops.forEach((stop) => stop());
    });
  });

  // A request or the event stream refused after sign-in means the access token ran out early or
  // this browser was signed out from another one: ask for a new token, else show the sign-in page.
  $effect(() => {
    if (
      session.mode === "signed-in" &&
      (live.problem === "unauthorized" || live.stream === "refused")
    ) {
      void session.recover();
    }
  });

  // Changing this browser's switch (or the app's own) is told at once, not at the next report.
  $effect(() => {
    void tabNotifications.active;
    void globalSettings.view?.ui.notifications;
    if (session.ready) untrack(() => reporter.refresh());
  });

  // A new report (the first read, or after `network_changed`) raises or withdraws the notice.
  $effect(() => {
    watcher.network(networkHealth.report);
  });
</script>

<svelte:head>
  <title>{documentTitle(page.url.pathname, live.open)}</title>
</svelte:head>

{#if session.mode === "checking"}
  <p class="visually-hidden" role="status">Loading.</p>
{:else if !session.ready}
  <SignInGate {session} />
{:else}
  <a class="skip-link" href="#main">Skip to content</a>
  <div class="shell">
    {#if !welcome}<Sidebar />{/if}
    <div class="content">
      {#if live.problem === "unauthorized"}
        <p class="banner" role="status">
          Puddle can't sign in to its service. Open Puddle from its app window
          or tray icon.
        </p>
      {:else if live.problem === "unreachable"}
        <p class="banner" role="status">
          Puddle's service isn't answering. Retrying.
        </p>
      {:else if live.problem === "failing"}
        <p class="banner" role="status">
          {live.problemDetail} Retrying.
        </p>
      {:else if live.stream === "refused" && live.streamRefusal}
        <p class="banner" role="status">
          Live updates stopped: {live.streamRefusal.reason}. {live.streamRefusal
            .advice}
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
{/if}

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
