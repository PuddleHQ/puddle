<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import SettingRow from "#lib/components/SettingRow.svelte";
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import SwitchRow from "#lib/components/SwitchRow.svelte";
  import ValueRow from "#lib/components/ValueRow.svelte";
  import { saveUi } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import { parseSshAgentPipe } from "#lib/settings/ssh-agent.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";
  import { identities } from "#lib/stores/identities.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";

  const SSH_HOW =
    "Used for SSH to servers other than Git hosts. The default is the OpenSSH agent's pipe \\\\.\\pipe\\openssh-ssh-agent on Windows, and the SSH_AUTH_SOCK of Puddle's session elsewhere. Pageant and 1Password users name the agent's own pipe here.";
  const saver = requireSaver();

  onMount(() => void identities.ensureLoaded());

  /** The two Git switches new workspaces start with; a refusal puts the box back and says why. */
  async function setGitDefault(
    key: "only_push_listed" | "only_pull_listed",
    on: boolean,
  ): Promise<boolean> {
    const result = await identities.setGitDefaults({ [key]: on });
    if (!result.ok) toasts.push(result.message, { tone: "error" });
    return result.ok;
  }
</script>

{#if store.view}
  {@const view = store.view}
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
    {@const defaults = identities.gitDefaults}
    <SwitchRow
      id="set-push-listed"
      label="Only push to allowed repositories"
      desc={defaults === null
        ? "Reading the current setting."
        : defaults.only_push_listed
          ? "New workspaces may push only to a repository whose Push box is ticked on their Git tab. This checks Git pushes only: a token in the workspace can still change a repository through the GitHub API."
          : "Off: a push may go to any repository the credential can write to. Each workspace can change this on its Git tab."}
      checked={defaults?.only_push_listed ?? false}
      disabled={defaults === null}
      onchange={(next) => setGitDefault("only_push_listed", next)}
    />
    <SwitchRow
      id="set-pull-listed"
      label="Only pull from allowed repositories"
      desc={defaults === null
        ? "Reading the current setting."
        : defaults.only_pull_listed
          ? "New workspaces may fetch only from a repository whose Pull box is ticked on their Git tab."
          : "Off: a fetch may go to any repository the credential can read. Each workspace can change this on its Git tab."}
      checked={defaults?.only_pull_listed ?? false}
      disabled={defaults === null}
      onchange={(next) => setGitDefault("only_pull_listed", next)}
    />
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
