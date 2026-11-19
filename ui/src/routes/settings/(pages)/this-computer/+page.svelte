<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import AuditRetention from "#lib/components/AuditRetention.svelte";
  import BackupCard from "#lib/components/BackupCard.svelte";
  import DevicesPanel from "#lib/components/DevicesPanel.svelte";
  import SelectRow from "#lib/components/SelectRow.svelte";
  import SettingRow from "#lib/components/SettingRow.svelte";
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import UiSwitchRow from "#lib/components/UiSwitchRow.svelte";
  import { api } from "#lib/api/client.ts";
  import { saveUi } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import { CLOSE_OPTIONS, type CloseBehaviour } from "#lib/settings/model.ts";
  import { session } from "#lib/session/session.svelte.ts";
  import { DevicesStore } from "#lib/stores/devices.svelte.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";

  const saver = requireSaver();
  const devices = new DevicesStore();
  let version = $state<string | null>(null);
  let versionAsked = $state(false);
  let notices = $state<string | null>(null);
  let noticesFailed = $state(false);

  onMount(() => {
    void api
      .GET("/api/health")
      .then((r) => (version = r.data?.version ?? null))
      .catch(() => undefined)
      .finally(() => (versionAsked = true));
  });

  async function loadNotices() {
    if (notices !== null || noticesFailed) return;
    try {
      notices = (await import("../../../../../THIRD-PARTY-NOTICES.txt?raw"))
        .default;
    } catch {
      noticesFailed = true;
    }
  }
</script>

{#if store.view}
  {@const ui = store.view.ui}
  <SettingsGroup title="Startup and window">
    <UiSwitchRow
      id="set-login"
      pref="start_at_login"
      label="Start Puddle when I sign in"
      desc="Puddle opens in the tray, so requests reach you without a manual start. Off by default."
      how="On a desktop with no tray icon it opens minimized. Only the Windows app applies this for now; on other systems the choice is kept but nothing starts yet."
      fallback={false}
    />
    <SelectRow
      id="set-close"
      label="Closing the window"
      desc="In the tray, workspaces keep running so builds carry on."
      how="Quitting from the tray menu stops every workspace. A desktop with no tray icon (some Linux desktops) minimizes the window instead."
      value={ui.close_behaviour ?? "tray"}
      options={CLOSE_OPTIONS}
      onchange={(next) =>
        saveUi(
          saver,
          "set-close",
          "close_behaviour",
          next as CloseBehaviour,
          ui.close_behaviour ?? "tray",
        )}
    />
    <UiSwitchRow
      id="set-minimize"
      pref="minimize_to_tray"
      label="Minimizing hides Puddle in the tray"
      desc="Off: minimizing leaves Puddle in the taskbar."
      how="Without a tray icon on your desktop, minimizing always leaves it in the taskbar."
      fallback={false}
    />
  </SettingsGroup>

  <SettingsGroup title="Devices and sign-in" id="set-devices">
    <DevicesPanel {session} store={devices} />
  </SettingsGroup>

  <SettingsGroup title="Back up" id="set-backup">
    <BackupCard />
  </SettingsGroup>

  <SettingsGroup title="Activity log" id="set-audit">
    <AuditRetention />
  </SettingsGroup>

  <SettingsGroup title="Checks and errors">
    <SettingRow
      id="set-system-check"
      label="System check"
      desc="Checks virtualization, the hypervisor, the bundled runtime and a test start of a tiny workspace."
      plain
    >
      <a class="btn" href="/welcome/check?from=settings"
        >Run the system check again</a
      >
    </SettingRow>
    <SettingRow
      id="set-app-errors"
      label="App errors"
      desc="Errors Puddle's pages hit that no code expected, as Puddle's log has them."
      plain
    >
      <a class="btn" href="/settings/app-errors"
        >Details<span class="visually-hidden"> about app errors</span></a
      >
    </SettingRow>
  </SettingsGroup>

  <SettingsGroup title="Privacy and about" id="set-about">
    <p class="line" id="set-privacy">
      Puddle sends nothing about you or your use of it: no telemetry, and no
      update check. Traffic from inside your workspaces (including editor and
      extension telemetry) goes through your rules like any other request.
    </p>
    <p class="line">
      <b>Puddle{version ? ` ${version}` : ""}</b>{#if versionAsked && !version}
        <span class="muted">
          (version unavailable: Puddle's service didn't say)</span
        >{/if}
      <span class="muted"> &middot; AGPL-3.0-or-later</span>
    </p>
    <details ontoggle={() => void loadNotices()}>
      <summary>Third-party licences</summary>
      {#if notices !== null}
        <!-- A scrollable region must be reachable by keyboard. -->
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <pre
          class="notices"
          tabindex="0"
          aria-label="Third-party licences">{notices}</pre>
      {:else if noticesFailed}
        <p class="error">Couldn't load the licence texts.</p>
      {:else}
        <p class="muted">Loading&hellip;</p>
      {/if}
    </details>
  </SettingsGroup>
{/if}

<style>
  .line {
    margin: 0 0 var(--space-2);
  }
  .muted {
    color: var(--color-text-muted);
  }
  .error {
    color: var(--color-danger);
  }
  summary {
    color: var(--color-accent);
    cursor: pointer;
  }
  .notices {
    max-height: 20rem;
    overflow: auto;
    margin: var(--space-2) 0 0;
    padding: var(--space-3);
    background: var(--color-bg);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    white-space: pre-wrap;
  }
</style>
