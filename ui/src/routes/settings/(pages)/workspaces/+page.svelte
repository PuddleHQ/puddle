<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import DirectSshDialog from "#lib/components/DirectSshDialog.svelte";
  import SelectRow from "#lib/components/SelectRow.svelte";
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import SwitchRow from "#lib/components/SwitchRow.svelte";
  import { saveLayer, saveTools } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import { cpuCounts, CPUS_NOTE, cpuLabel } from "#lib/settings/cpus.ts";
  import { memorySizes, miseManaged } from "#lib/settings/model.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";
  import {
    DIRECT_SSH_HINT,
    DIRECT_SSH_DEFAULT_LABEL,
    TRUST_WORDS_GLOBAL,
  } from "#lib/workspaces/direct-ssh.ts";

  const saver = requireSaver();

  let trustOpen = $state(false);
</script>

{#if store.view}
  {@const view = store.view}
  {@const eff = view.effective}
  <SettingsGroup title="New workspaces">
    <SelectRow
      id="set-memory"
      label="Default memory"
      desc="New workspaces start with this; a workspace can set its own."
      restart
      value={String(eff.memory.value)}
      options={memorySizes(eff.memory.value).map((s) => ({
        value: String(s.value),
        label: s.label,
      }))}
      onchange={(next) =>
        saveLayer(
          saver,
          "set-memory",
          "memory",
          Number(next),
          eff.memory.value,
        )}
    />
    <SelectRow
      id="set-cpus"
      label="Default CPUs"
      desc="New workspaces start with this count; a workspace can set its own."
      how={CPUS_NOTE}
      restart
      value={String(eff.cpus.value)}
      options={cpuCounts(view.effective_limits.max_cpus, eff.cpus.value).map(
        (count) => ({ value: String(count), label: cpuLabel(count) }),
      )}
      onchange={(next) =>
        saveLayer(saver, "set-cpus", "cpus", Number(next), eff.cpus.value)}
    />
    {#if view.mise_available}
      <SwitchRow
        id="set-mise"
        label="Manage tools with mise in workspaces"
        desc="On by default. Applies to every workspace; no workspace can turn it back on."
        how="Turning this off leaves every workspace's tools and mise.toml as they are; running workspaces drop mise from PATH at their next start."
        checked={miseManaged(view)}
        onchange={(next) =>
          saveTools(saver, "set-mise", "mise", next, miseManaged(view))}
      />
    {/if}
    <SwitchRow
      id="set-direct-ssh"
      label={DIRECT_SSH_DEFAULT_LABEL}
      desc="Off by default. A workspace can set its own switch."
      how={DIRECT_SSH_HINT}
      checked={eff.direct_ssh.value}
      onchange={(next) => {
        if (next) {
          // Turning it on says the trust text first; the dialog's button saves it.
          trustOpen = true;
          return false;
        }
        return saveLayer(
          saver,
          "set-direct-ssh",
          "direct_ssh",
          false,
          eff.direct_ssh.value,
        );
      }}
    />
    <SelectRow
      id="set-clip"
      label="Clipboard reads by pages"
      desc="What new workspaces start with. Typing Ctrl+V always works; this is for pages that read the clipboard by themselves. Each workspace can change its own on its Settings tab."
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
{/if}

<DirectSshDialog
  bind:open={trustOpen}
  words={TRUST_WORDS_GLOBAL}
  onConfirm={() => {
    const eff = store.view?.effective;
    if (eff)
      void saveLayer(
        saver,
        "set-direct-ssh",
        "direct_ssh",
        true,
        eff.direct_ssh.value,
      );
  }}
/>
