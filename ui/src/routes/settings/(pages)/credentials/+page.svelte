<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import SwitchRow from "#lib/components/SwitchRow.svelte";
  import { saveLayer } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";

  const saver = requireSaver();
</script>

{#if store.view}
  {@const eff = store.view.effective}
  <SettingsGroup title="Logins and credentials">
    <SwitchRow
      id="set-capture"
      label="Keep logins made in workspaces"
      desc="Puddle keeps the real token; the workspace gets a stand-in. On by default."
      how="When you sign in to Claude Code, GitHub or Copilot inside a workspace, Puddle keeps the real token on this computer and the workspace gets a stand-in that works only through Puddle. When Puddle cannot keep a token, it stops the sign-in instead of letting the real token into the workspace. A workspace can set its own switch. Turning it off leaves the logins Puddle already kept unused, and a login then keeps its real token in the workspace. A token the workspace prints, such as the one of claude setup-token, is a stand-in as well and works only inside that workspace."
      restart
      checked={eff.capture_logins.value}
      onchange={(next) =>
        saveLayer(
          saver,
          "set-capture",
          "capture_logins",
          next,
          eff.capture_logins.value,
        )}
    />
    <SwitchRow
      id="set-foreign-warning"
      label="Warn about a credential Puddle did not add"
      desc="A notice and an Activity flag when a tool sends a token Puddle does not hold. On by default."
      how="The request still goes through as it always did. The flag names the website and what kind of token it looked like, never the token itself. Puddle cannot tell your own token from one that leaked, so this catches mistakes, not a workspace that hides what it sends. A workspace can set its own switch. The same switch covers a token of a known kind that a website sends the workspace in an answer and Puddle does not hold."
      checked={eff.warn_foreign_credentials.value}
      onchange={(next) =>
        saveLayer(
          saver,
          "set-foreign-warning",
          "warn_foreign_credentials",
          next,
          eff.warn_foreign_credentials.value,
        )}
    />
  </SettingsGroup>
{/if}
