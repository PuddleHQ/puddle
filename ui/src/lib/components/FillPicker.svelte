<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import {
    SWATCHES,
    fillVars,
    inkOn,
    isHexColour,
    solid,
    type Fill,
  } from "#lib/rules/fill.ts";
  import { contrast } from "#lib/theme/palette.ts";

  // The colour picker of the rule set dialog: a row of swatches, a free choice from the system
  // colour control, and a preview of the set's name on its colour with the contrast it reaches.
  // `value` is the chosen fill, or `null` to leave the colour to Puddle (a new set gets one).
  let {
    value = $bindable(null),
    name = "",
  }: { value?: Fill | null; name?: string } = $props();

  const id = $props.id();
  const current = $derived(value?.color ?? null);
  const preview = $derived(name.trim() === "" ? "Rule set" : name.trim());
  const ratio = $derived(
    current && isHexColour(current)
      ? contrast(current, inkOn(current)).toFixed(1)
      : null,
  );

  function pick(hex: string) {
    value = solid(hex);
  }
</script>

<fieldset class="picker">
  <legend>Colour</legend>
  <div class="swatches" role="group" aria-label="Colour swatches">
    {#each SWATCHES as hex (hex)}
      <button
        type="button"
        class="swatch"
        aria-pressed={current === hex}
        aria-label={hex}
        style="--swatch: {hex}"
        onclick={() => pick(hex)}
      ></button>
    {/each}
    <label class="custom" for="{id}-custom">
      <span class="label">Other</span>
      <input
        id="{id}-custom"
        type="color"
        value={current ?? SWATCHES[0]}
        oninput={(e) => pick(e.currentTarget.value)}
      />
    </label>
  </div>
  {#if value && current}
    <p class="preview">
      <span class="sample" style={fillVars(value)}>{preview}</span>
      <span class="hint">
        {current}, text contrast {ratio}:1
      </span>
    </p>
  {:else}
    <p class="hint">Puddle picks a colour; choose one to use your own.</p>
  {/if}
</fieldset>

<style>
  .picker {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    border: 0;
  }
  legend {
    padding: 0;
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .swatches {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  .swatch {
    width: var(--control-size);
    height: var(--control-size);
    padding: 0;
    border: 1px solid var(--color-border);
    border-radius: 50%;
    background: var(--swatch);
    cursor: pointer;
  }
  .swatch[aria-pressed="true"] {
    outline: 3px solid var(--color-text);
    outline-offset: 2px;
  }
  .swatch:focus-visible {
    outline: 3px solid var(--color-focus);
    outline-offset: 2px;
  }
  .custom {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
  }
  .label,
  .hint {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .custom input {
    width: var(--control-size);
    height: var(--control-size);
    padding: 0;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
  }
  .preview {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    margin: 0;
  }
  .sample {
    display: inline-block;
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    background: var(--set-fill);
    color: var(--set-ink);
    font-size: var(--text-sm);
    font-weight: 600;
  }
</style>
