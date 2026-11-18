<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import {
    LEARNING_NOTE,
    MODE_NAMES,
    MODE_WARNINGS,
    MODES,
    type Mode,
  } from "#lib/interception/modes.ts";
  import { useSaver } from "#lib/settings/context.ts";
  import { tracked } from "#lib/settings/saver.svelte.ts";
  import { GlobalInterception } from "#lib/stores/interception.svelte.ts";
  import SelectRow from "./SelectRow.svelte";
  import SwitchRow from "./SwitchRow.svelte";
  import "#lib/theme/controls.css";

  // The mode every workspace without its own follows, whether Puddle learns from rejections, and
  // the built-in list mode 2 passes through. A running workspace applies a change at once.
  const store = new GlobalInterception();
  const saver = useSaver();

  $effect(() => {
    void store.load();
  });

  const choose = (mode: string) => {
    const previous = store.mode;
    return tracked(
      saver,
      "set-interception-mode",
      () => store.setMode(mode as Mode),
      {
        undo: () => store.setMode(previous),
      },
    );
  };

  const learning = (on: boolean) => {
    const previous = store.learn;
    return tracked(
      saver,
      "set-interception-learn",
      () => store.setLearning(on),
      {
        undo: () => store.setLearning(previous),
      },
    );
  };
</script>

{#if store.status === "failed"}
  <div role="alert">
    <p class="muted">Couldn't read the interception setting.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else if store.status === "ready"}
  <SelectRow
    id="set-interception-mode"
    label="Which hosts to decrypt"
    desc={MODE_WARNINGS[store.mode]}
    how="Workspaces follow this unless they have a mode of their own. Hosts with a credential, secret, login capture or rule on method and path are always decrypted.{store.chosen
      ? ''
      : ` You haven't chosen yet: until you do, Puddle decrypts only hosts it has a reason to read.`}"
    value={store.mode}
    options={MODES.map((mode) => ({ value: mode, label: MODE_NAMES[mode] }))}
    onchange={choose}
  />
  <SwitchRow
    id="set-interception-learn"
    label="Learn from rejections"
    desc="Pass a host through for 12 hours when a tool refuses Puddle's certificate."
    how="{LEARNING_NOTE} It only matters in the modes that decrypt unlisted hosts."
    checked={store.learn}
    onchange={learning}
  />
  <details class="curated">
    <summary
      >Passed through in "except a known list" ({store.curated.length})</summary
    >
    {#if store.curated.length === 0}
      <p class="muted">
        The list is empty for now. Puddle adds a host here once it is known to
        refuse its certificate; until then this mode behaves like decrypting
        every host.
      </p>
    {:else}
      <ul class="names">
        {#each store.curated as name (name)}<li>{name}</li>{/each}
      </ul>
    {/if}
  </details>
{/if}

<style>
  .muted {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .error {
    margin: 0;
    color: var(--color-danger);
  }
  .curated {
    padding-block: var(--space-2);
  }
  .curated summary {
    color: var(--color-accent);
    cursor: pointer;
  }
  .names {
    margin: var(--space-1) 0 0;
    padding-left: var(--space-4);
  }
</style>
