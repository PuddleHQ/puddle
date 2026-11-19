<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { workspaces as store } from "#lib/stores/workspaces.svelte.ts";
  import "#lib/theme/controls.css";

  // A dropdown of workspaces: the value is the generated id, what a person reads is the name. The id
  // only shows next to a name another workspace shares. `known` adds ids the list may not hold (a
  // rule that names a deleted workspace), shown as the bare id because nothing else is left.
  let {
    id,
    value = $bindable(""),
    known = [],
    leading,
    invalid = false,
    describedby,
  }: {
    id: string;
    value?: string;
    known?: readonly string[];
    /** Extra first options (for a filter: "Any", "Every workspace"); the default is "Choose a workspace". */
    leading?: readonly { value: string; label: string }[];
    invalid?: boolean;
    describedby?: string | undefined;
  } = $props();

  const options = $derived(
    [...store.list.map((w) => w.id), ...known]
      .filter((one, at, all) => all.indexOf(one) === at)
      .map((one) => ({ value: one, label: store.spokenLabelOf(one) }))
      .sort((a, b) => a.label.localeCompare(b.label)),
  );
</script>

<select
  {id}
  bind:value
  aria-invalid={invalid ? "true" : undefined}
  aria-describedby={describedby}
>
  {#if leading}
    {#each leading as option (option.value)}
      <option value={option.value}>{option.label}</option>
    {/each}
  {:else}
    <option value="">Choose a workspace</option>
  {/if}
  {#each options as option (option.value)}
    <option value={option.value}>{option.label}</option>
  {/each}
</select>
