<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { Identity } from "#lib/identities/model.ts";
  import type { RepoView } from "#lib/repos/model.ts";
  import { resolveRepoAddress } from "#lib/workspaces/model.ts";
  import RepoPicker from "./RepoPicker.svelte";

  // The repository address field every "add a repository" form shares: the label and the address
  // box, the refusal or the hint under it (an `owner/repo` shorthand says which address it means),
  // and the search over the repositories the identities reach. The form owns the checks; this only
  // shows `error` and takes what is typed or picked.
  let {
    id,
    value = $bindable(""),
    label,
    identities,
    error = null,
    hint,
    picker = true,
    oninput = undefined,
    onblur = undefined,
    onPick,
  }: {
    /** The address input's id; the refusal is `<id>-error` and the hint `<id>-hint`. */
    id: string;
    value?: string;
    label: string;
    identities: readonly Identity[];
    /** The refusal to show now, or `null`. */
    error?: string | null;
    /** What the field accepts, shown while there is no refusal and no shorthand to expand. */
    hint: string;
    /** Whether the "Choose from your repositories" search shows. */
    picker?: boolean;
    oninput?: () => void;
    onblur?: () => void;
    onPick: (repo: RepoView, listedBy: number | null) => void;
  } = $props();

  const resolved = $derived(resolveRepoAddress(value));
  const willClone = $derived(
    "url" in resolved && resolved.url !== value.trim()
      ? `Will clone ${resolved.url}`
      : null,
  );
</script>

<div class="field">
  <label for={id}>{label}</label>
  <input
    {id}
    type="text"
    inputmode="url"
    autocomplete="off"
    autocapitalize="off"
    spellcheck="false"
    placeholder="https://github.com/you/project.git"
    bind:value
    {oninput}
    {onblur}
    aria-invalid={error ? "true" : undefined}
    aria-describedby={error ? `${id}-error` : `${id}-hint`}
  />
  {#if error}
    <p class="error" id="{id}-error" role="alert">{error}</p>
  {:else}
    <p class="hint" id="{id}-hint" aria-live="polite">{willClone ?? hint}</p>
  {/if}
</div>

{#if picker}
  <RepoPicker {identities} {onPick} />
{/if}

<style>
  .field {
    display: grid;
    gap: var(--space-1);
  }
  .hint,
  .error {
    margin: 0;
    font-size: var(--text-sm);
  }
  .hint {
    color: var(--color-text-muted);
  }
  .error {
    color: var(--color-danger);
  }
</style>
