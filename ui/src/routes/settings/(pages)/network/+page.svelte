<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import GlobalInterception from "#lib/components/GlobalInterception.svelte";
  import GlobalNeverDecrypt from "#lib/components/GlobalNeverDecrypt.svelte";
  import LearnedLimitsSetting from "#lib/components/LearnedLimitsSetting.svelte";
  import ProxySignIn from "#lib/components/ProxySignIn.svelte";
  import SettingRow from "#lib/components/SettingRow.svelte";
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import SwitchRow from "#lib/components/SwitchRow.svelte";
  import {
    LOCAL_LABELS,
    type LocalCategory,
  } from "#lib/decision/local-labels.ts";
  import { saveLayer, saveToggle } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import { LOCAL_EXPLAIN } from "#lib/settings/model.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";

  const saver = requireSaver();
  const CATEGORIES: LocalCategory[] = [
    "loopback",
    "private",
    "company_network",
    "link_local",
    "metadata",
    "special",
  ];

  /** The first sentence of an explanation is the row's line; the rest sits behind "How this works". */
  function split(text: string): [string, string | null] {
    const [first = "", ...rest] = text.split(/(?<=\.)\s+/);
    return [first, rest.length > 0 ? rest.join(" ") : null];
  }
</script>

{#if store.view}
  {@const eff = store.view.effective}
  <SettingsGroup title="Proxy">
    <SettingRow
      id="set-net-health"
      label="Network health"
      desc="The proxy Puddle found and the company certificates it uses."
      plain
    >
      <a class="btn" href="/settings/network-health">
        Details<span class="visually-hidden"> about network health</span>
      </a>
    </SettingRow>
    <div id="set-proxy-sign-in"><ProxySignIn /></div>
  </SettingsGroup>

  <SettingsGroup
    title="Local destinations"
    id="local-destinations"
    intro="Off by default. Turning one on makes those destinations approvable: each one still needs a rule or your approval."
  >
    {#each CATEGORIES as category (category)}
      {@const [line, more] = split(LOCAL_EXPLAIN[category] ?? "")}
      <SwitchRow
        id="set-local-{category}"
        label={LOCAL_LABELS[category]}
        desc={line}
        how={more}
        checked={eff.local_toggles[category].value}
        onchange={(next) =>
          saveToggle(
            saver,
            `set-local-${category}`,
            category,
            next,
            eff.local_toggles[category].value,
          )}
      />
    {/each}
    <SwitchRow
      id="set-wild"
      label="Let suffix rules reach local addresses"
      desc="Off: .example.com never matches a name that resolves to a local address."
      how="With it off, local destinations need an exact rule."
      checked={eff.wildcards_reach_local.value}
      onchange={(next) =>
        saveLayer(
          saver,
          "set-wild",
          "wildcards_reach_local",
          next,
          eff.wildcards_reach_local.value,
        )}
    />
  </SettingsGroup>

  <SettingsGroup title="Decrypting traffic">
    <GlobalInterception />
    <div id="set-never-decrypt"><GlobalNeverDecrypt /></div>
    <div id="set-learned-limits"><LearnedLimitsSetting /></div>
  </SettingsGroup>
{/if}
