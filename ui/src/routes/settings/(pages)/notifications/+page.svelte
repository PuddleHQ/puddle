<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import TabNotificationsSetting from "#lib/components/TabNotificationsSetting.svelte";
  import UiSwitchRow from "#lib/components/UiSwitchRow.svelte";
  import ValueRow from "#lib/components/ValueRow.svelte";
  import { inDesktopApp } from "#lib/desktop/folder-picker.ts";
  import { saveUi } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import {
    DISK_WARNING_DEFAULT,
    parseDiskWarning,
  } from "#lib/settings/model.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";

  const saver = requireSaver();
</script>

{#if store.view}
  {@const ui = store.view.ui}
  <SettingsGroup title="Notifications">
    <UiSwitchRow
      id="set-notify"
      pref="notifications"
      label="System notifications for new requests"
      desc="One at most every 10 seconds per workspace; clicking it opens the request."
      fallback={true}
    />
    {#if !inDesktopApp()}<TabNotificationsSetting />{/if}
    <UiSwitchRow
      id="set-unfocused"
      pref="notify_only_unfocused"
      label="Only when Puddle is not the window in front"
      desc="Off: a notification also shows while you are looking at Puddle."
      fallback={true}
      disabled={!(ui.notifications ?? true)}
    />
    <UiSwitchRow
      id="set-sound"
      pref="sound"
      label="Sound"
      desc="Play the system notification sound."
      fallback={false}
    />
    <UiSwitchRow
      id="set-badge"
      pref="badge"
      label="Show the number of waiting requests"
      desc="On the tray icon, the taskbar and the window title."
      fallback={true}
    />
  </SettingsGroup>

  <SettingsGroup title="Also notify when">
    <UiSwitchRow
      id="set-oom"
      pref="notify_oom"
      label="A workspace runs out of memory"
      desc="It was stopped because it used all the memory it was given."
      fallback={true}
    />
    <UiSwitchRow
      id="set-disk-full"
      pref="notify_disk_full"
      label="A workspace's disk is nearly full"
      desc="Its root disk or volume reached the percentage below."
      how="The percentage is of the disk's own size, as the disk file takes it on the drive, not the room left there."
      fallback={true}
    />
    <ValueRow
      id="set-disk-percent"
      label="Warn when a disk is this full (%)"
      desc="The warning shows on the workspace even with the notification off."
      value={String(ui.disk_warning_percent ?? DISK_WARNING_DEFAULT)}
      parse={(text) => {
        const parsed = parseDiskWarning(text);
        return parsed.ok ? { ok: true, value: parsed.percent } : parsed;
      }}
      onsave={(percent) =>
        saveUi(
          saver,
          "set-disk-percent",
          "disk_warning_percent",
          percent,
          ui.disk_warning_percent ?? DISK_WARNING_DEFAULT,
        )}
      inputmode="numeric"
    />
    <UiSwitchRow
      id="set-stopped"
      pref="notify_unexpected_stop"
      label="A workspace stops on its own"
      desc="It stopped or crashed without you asking."
      fallback={true}
    />
    <UiSwitchRow
      id="set-low-disk"
      pref="notify_low_disk_space"
      label="The drive that holds Puddle's data runs low on space"
      desc="Once for each free-space line it crosses (see Disk space under Workspaces)."
      fallback={true}
    />
  </SettingsGroup>

  <SettingsGroup title="Inbox">
    <UiSwitchRow
      id="set-confirm-global"
      pref="confirm_global_rules"
      label="Confirm before a rule affects every workspace"
      desc="Allowing or denying for every workspace asks first."
      how="Turn this off to decide at once; you can still undo right after."
      fallback={true}
    />
    <UiSwitchRow
      id="set-inbox-keys"
      pref="inbox_shortcuts"
      label="Keyboard shortcuts in the inbox"
      desc="A allows, D denies, J and K move between requests."
      how="Turn this off if a stray key press could decide a request; Tab, Enter and Space always work."
      fallback={true}
    />
  </SettingsGroup>
{/if}
