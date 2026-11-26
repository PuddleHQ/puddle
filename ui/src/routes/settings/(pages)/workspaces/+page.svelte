<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import DirectSshDialog from "#lib/components/DirectSshDialog.svelte";
  import EnvironmentTable from "#lib/components/EnvironmentTable.svelte";
  import SelectRow from "#lib/components/SelectRow.svelte";
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import SwitchRow from "#lib/components/SwitchRow.svelte";
  import ValueRow from "#lib/components/ValueRow.svelte";
  import { SECRET_NEVER_IN } from "#lib/environment/model.ts";
  import {
    saveLayer,
    saveLimits,
    saveTools,
    saveUi,
  } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import { cpuCounts, CPUS_NOTE, cpuLabel } from "#lib/settings/cpus.ts";
  import {
    DEFAULT_FAIL_FREE_GB,
    DEFAULT_WARN_FREE_GB,
    linesProblem,
    parseFreeGb,
  } from "#lib/settings/disk-space.ts";
  import { memorySizes, miseManaged } from "#lib/settings/model.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";
  import { EnvironmentStore } from "#lib/stores/environment.svelte.ts";
  import {
    DIRECT_SSH_HINT,
    DIRECT_SSH_DEFAULT_LABEL,
    TRUST_WORDS_GLOBAL,
  } from "#lib/workspaces/direct-ssh.ts";

  const saver = requireSaver();
  const env = new EnvironmentStore();
  onMount(() => env.start({ kind: "global" }));

  let trustOpen = $state(false);

  /** A free-space line as typed, checked against the other line as it is stored. */
  function freeLine(other: () => number, isWarn: boolean) {
    return (text: string) => {
      const parsed = parseFreeGb(text);
      if (!parsed.ok) return parsed;
      const problem = isWarn
        ? linesProblem(parsed.gb, other())
        : linesProblem(other(), parsed.gb);
      return problem
        ? { ok: false as const, message: problem }
        : { ok: true as const, value: parsed.gb };
    };
  }
</script>

{#if store.view}
  {@const view = store.view}
  {@const eff = view.effective}
  {@const warnStored = view.ui.disk_warn_free_gb ?? DEFAULT_WARN_FREE_GB}
  {@const failStored = view.ui.disk_fail_free_gb ?? DEFAULT_FAIL_FREE_GB}
  <SettingsGroup title="Workspaces">
    <SelectRow
      id="set-memory"
      label="Default memory"
      desc="Used by every workspace that has no value of its own."
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
      desc="Used by every workspace that has no count of its own."
      how={CPUS_NOTE}
      restart
      value={String(eff.cpus.value)}
      options={cpuCounts(view.effective_limits.max_cpus, eff.cpus.value).map(
        (count) => ({ value: String(count), label: cpuLabel(count) }),
      )}
      onchange={(next) =>
        saveLayer(saver, "set-cpus", "cpus", Number(next), eff.cpus.value)}
    />
    <ValueRow
      id="set-max-cpus"
      label="Most CPUs a workspace may have"
      desc="Leave empty to allow as many as this computer has ({view
        .effective_limits.host_cpus})."
      how="The default and every workspace's own count stay within it."
      value={String(view.limits.max_cpus ?? "")}
      placeholder={String(view.effective_limits.host_cpus)}
      inputmode="numeric"
      parse={(text) => {
        const trimmed = text.trim();
        if (trimmed === "") return { ok: true, value: null };
        const count = Number(trimmed);
        return Number.isInteger(count) && count >= 1 && count <= 255
          ? { ok: true, value: count }
          : { ok: false, message: "Use a whole number from 1 to 255." };
      }}
      onsave={(count) =>
        saveLimits(
          saver,
          "set-max-cpus",
          "max_cpus",
          count,
          view.limits.max_cpus ?? null,
        )}
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

  <SettingsGroup
    title="Disk space"
    intro="Free room on the drive that holds Puddle's data, in GB of 1024 x 1024 x 1024 bytes."
  >
    <ValueRow
      id="set-disk-warn"
      label="Warn when free space is under (GB)"
      desc="Default {DEFAULT_WARN_FREE_GB}. The check shows a warning and the app says so once."
      how="puddle doctor and the app check the room left on the drive against these two lines. They do not measure the size of the disks: a workspace's root disk (16 GiB for a new workspace) and its volume are sparse files that take space only as the workspace writes, so a workspace can still grow past what is left."
      value={String(warnStored)}
      inputmode="numeric"
      parse={freeLine(() => failStored, true)}
      onsave={(gb) =>
        saveUi(saver, "set-disk-warn", "disk_warn_free_gb", gb, warnStored)}
    />
    <ValueRow
      id="set-disk-fail"
      label="Fail when free space is under (GB)"
      desc="Default {DEFAULT_FAIL_FREE_GB}. The check fails and the app says so once. Never above the warning line."
      value={String(failStored)}
      inputmode="numeric"
      parse={freeLine(() => warnStored, false)}
      onsave={(gb) =>
        saveUi(saver, "set-disk-fail", "disk_fail_free_gb", gb, failStored)}
    />
  </SettingsGroup>

  <SettingsGroup title="Environment" id="set-env">
    <p class="line">
      Variables and secrets every workspace gets at its next start, unless the
      workspace has its own of the same name. Changes here are saved at once.
    </p>
    <details>
      <summary>Show the variables</summary>
      <p class="line">{SECRET_NEVER_IN}</p>
      <EnvironmentTable store={env} label="Environment of every workspace" />
    </details>
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

<style>
  .line {
    margin: 0 0 var(--space-2);
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  summary {
    color: var(--color-accent);
    cursor: pointer;
  }
</style>
