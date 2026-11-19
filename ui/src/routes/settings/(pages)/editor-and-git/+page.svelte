<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import MicrosoftServerDialog from "#lib/components/MicrosoftServerDialog.svelte";
  import SelectRow from "#lib/components/SelectRow.svelte";
  import SettingRow from "#lib/components/SettingRow.svelte";
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import SwitchRow from "#lib/components/SwitchRow.svelte";
  import ValueRow from "#lib/components/ValueRow.svelte";
  import { saveLayer, saveServer, saveUi } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import { tracked } from "#lib/settings/saver.svelte.ts";
  import { parseSshAgentPipe } from "#lib/settings/ssh-agent.ts";
  import {
    microsoftChosenToast,
    needsMicrosoftConsent,
    parseGrace,
    serverNote,
    serverOf,
    type ServerChoice,
  } from "#lib/settings/model.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";

  const SSH_HOW =
    "Used for SSH to servers other than Git hosts. The default is the OpenSSH agent's pipe \\\\.\\pipe\\openssh-ssh-agent on Windows, and the SSH_AUTH_SOCK of Puddle's session elsewhere. Pageant and 1Password users name the agent's own pipe here.";
  const saver = requireSaver();
  let popupOpen = $state(false);
  let popupBusy = $state(false);
  let popupError = $state<string | null>(null);
  let withdrawing = $state(false);

  const when = (ms: number) =>
    new Intl.DateTimeFormat(undefined, { dateStyle: "medium" }).format(ms);

  function chooseServer(next: string) {
    const view = store.view;
    const consent = store.consents?.vscode_server;
    if (!view || !consent) return false;
    const choice = next as ServerChoice;
    if (choice === "microsoft" && needsMicrosoftConsent(consent)) {
      // The terms come first; the popup's button saves the choice.
      popupError = null;
      popupOpen = true;
      return false;
    }
    return saveServer(saver, "set-server", "server", choice, serverOf(view));
  }

  async function accept(telemetry: boolean) {
    popupBusy = true;
    popupError = null;
    const result = await tracked(saver, "set-server", () =>
      store.grantMicrosoft(telemetry),
    );
    popupBusy = false;
    if (result) {
      popupOpen = false;
      toasts.push(microsoftChosenToast(telemetry), { ms: 12_000 });
    } else {
      const row = saver?.rows["set-server"];
      popupError =
        row?.kind === "failed" ? row.message : "Puddle couldn't save that.";
    }
  }

  async function withdraw() {
    withdrawing = true;
    await tracked(saver, "set-server", () => store.withdrawMicrosoft(), {
      note: "Consent withdrawn. Workspaces use code-server when they next start.",
    });
    withdrawing = false;
  }
</script>

{#if store.view && store.consents}
  {@const view = store.view}
  {@const eff = view.effective}
  {@const server = serverOf(view)}
  {@const consent = store.consents.vscode_server}
  <SettingsGroup title="Browser VS Code">
    <SelectRow
      id="set-server"
      label="Server"
      desc={serverNote(server)}
      value={server}
      options={[
        { value: "code_server", label: "code-server (bundled)" },
        { value: "microsoft", label: "Microsoft's VS Code server" },
      ]}
      onchange={chooseServer}
    />
    {#if consent.state === "granted"}
      <SettingRow
        id="set-withdraw"
        label="Microsoft's terms"
        desc="You accepted Microsoft's terms on {when(consent.at)}."
        plain
      >
        <button
          type="button"
          class="btn"
          disabled={withdrawing}
          onclick={() => void withdraw()}>Withdraw consent</button
        >
      </SettingRow>
    {/if}
    {#if server === "microsoft"}
      <SwitchRow
        id="set-ms-telemetry"
        label="Send Microsoft telemetry"
        desc="Off adds --disable-telemetry."
        checked={view.vscode_server.telemetry ?? false}
        onchange={(next) =>
          saveServer(
            saver,
            "set-ms-telemetry",
            "telemetry",
            next,
            view.vscode_server.telemetry ?? false,
          )}
      />
    {/if}
    <SwitchRow
      id="set-update"
      label="Update the server automatically"
      desc="Only switches when no window of that workspace is open."
      checked={view.vscode_server.auto_update ?? true}
      onchange={(next) =>
        saveServer(
          saver,
          "set-update",
          "auto_update",
          next,
          view.vscode_server.auto_update ?? true,
        )}
    />
    <ValueRow
      id="set-grace"
      label="Reconnection grace (seconds)"
      desc="How long a closed window's session is kept for a reconnect."
      value={String(eff.reconnection_grace.value)}
      inputmode="numeric"
      parse={(text) => {
        const parsed = parseGrace(text);
        return parsed.ok ? { ok: true, value: parsed.secs } : parsed;
      }}
      onsave={(secs) =>
        saveLayer(
          saver,
          "set-grace",
          "reconnection_grace",
          secs,
          eff.reconnection_grace.value,
        )}
    />
    <SwitchRow
      id="set-zoom"
      label="Zoom shortcuts"
      desc="Ctrl and plus or minus in workspace windows."
      checked={eff.zoom_hotkeys.value}
      onchange={(next) =>
        saveLayer(
          saver,
          "set-zoom",
          "zoom_hotkeys",
          next,
          eff.zoom_hotkeys.value,
        )}
    />
    <SelectRow
      id="set-clip"
      label="Clipboard reads by pages"
      desc="Typing Ctrl+V always works; this is for pages that read the clipboard by themselves."
      value={eff.clipboard_read.value}
      options={[
        { value: "ask", label: "Ask once per workspace" },
        { value: "allow", label: "Always allow" },
        { value: "deny", label: "Never allow" },
      ]}
      onchange={(next) =>
        saveLayer(
          saver,
          "set-clip",
          "clipboard_read",
          next as "ask" | "allow" | "deny",
          eff.clipboard_read.value,
        )}
    />
  </SettingsGroup>

  <SettingsGroup title="Git and SSH">
    <SettingRow
      id="set-identities"
      label="Git and credentials"
      desc="The Git name and email, and the credentials Puddle adds to requests, are kept as identities."
      how="The secret of a credential never enters a workspace."
      plain
    >
      <a class="btn" href="/identities">Manage identities</a>
    </SettingRow>
    <ValueRow
      id="set-ssh-agent"
      label="SSH agent pipe or socket"
      desc="The agent puddle doctor asks for the key list. Empty uses the default."
      how={SSH_HOW}
      value={view.ui.ssh_agent_pipe ?? ""}
      width="wide"
      mono
      parse={(text) => {
        const parsed = parseSshAgentPipe(text);
        return parsed.ok ? { ok: true, value: parsed.pipe } : parsed;
      }}
      onsave={(pipe) =>
        saveUi(
          saver,
          "set-ssh-agent",
          "ssh_agent_pipe",
          pipe,
          view.ui.ssh_agent_pipe ?? null,
        )}
    />
  </SettingsGroup>
{/if}

<MicrosoftServerDialog
  bind:open={popupOpen}
  busy={popupBusy}
  error={popupError}
  onAccept={(telemetry) => void accept(telemetry)}
/>
