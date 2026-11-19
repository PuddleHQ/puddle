<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // This browser's own switch for notifications about new requests. Not part of the saved
  // settings: every browser, phone and tab group chooses for itself, and the choice is kept in
  // the browser. The desktop app has its own system notifications (the switches above).
  import SettingRow from "./SettingRow.svelte";
  import {
    tabNotifications,
    type EnableResult,
  } from "#lib/notify/tab-switch.svelte.ts";

  let problem = $state<EnableResult | null>(null);

  async function change(box: HTMLInputElement) {
    if (!box.checked) {
      tabNotifications.disable();
      problem = null;
      return;
    }
    const result = await tabNotifications.enable();
    problem = result === "on" ? null : result;
    // A refusal leaves the switch where it was, whatever the click did to the box.
    box.checked = tabNotifications.active;
  }
</script>

<SettingRow
  id="set-tab-notify"
  label="Notifications in this browser"
  desc="A notification in this browser when a workspace asks for access."
  how="Only one of your open Puddle pages is told: the one you used in the last two minutes, or all of them when you used none. Clicking it opens the request. Needs the browser's permission, and an address the browser trusts (https:// or localhost)."
>
  <input
    id="set-tab-notify"
    type="checkbox"
    role="switch"
    aria-describedby="set-tab-notify-desc"
    checked={tabNotifications.active}
    onchange={(e) => void change(e.currentTarget)}
  />
  {#if problem === "denied" || (tabNotifications.chosen && tabNotifications.permission === "denied")}
    <p class="error" id="set-tab-notify-problem" role="alert">
      This browser blocked notifications for this page. Allow them in the site's
      settings, then turn this on again.
    </p>
  {:else if problem === "unsupported"}
    <p class="error" id="set-tab-notify-problem" role="alert">
      This browser can't show notifications for this page.
    </p>
  {/if}
</SettingRow>

<style>
  .error {
    margin: 0;
    max-width: 24rem;
    color: var(--color-danger);
    font-size: var(--text-sm);
    text-align: end;
  }
</style>
