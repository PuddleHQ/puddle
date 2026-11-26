<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import EnvironmentTable from "#lib/components/EnvironmentTable.svelte";
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import ValueRow from "#lib/components/ValueRow.svelte";
  import { SECRET_NEVER_IN } from "#lib/environment/model.ts";
  import { saveLimits } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";
  import { EnvironmentStore } from "#lib/stores/environment.svelte.ts";

  const saver = requireSaver();
  const env = new EnvironmentStore();
  onMount(() => env.start({ kind: "global" }));
</script>

{#if store.view}
  {@const view = store.view}
  <SettingsGroup title="Limits">
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
