<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { palette } from "#lib/theme/palette.svelte.ts";
  import { isPaletteChoice } from "#lib/theme/palette.ts";
  import { density } from "#lib/theme/density.svelte.ts";
  import { isDensityChoice } from "#lib/theme/density.ts";
  import { theme } from "#lib/theme/theme.svelte.ts";
  import { isThemeChoice } from "#lib/theme/theme.ts";
  import {
    DENSITY_OPTIONS,
    PALETTE_PRESET_OPTIONS,
    THEME_OPTIONS,
  } from "#lib/welcome/look.ts";
  import StepHeading from "#lib/welcome/StepHeading.svelte";
  import { around } from "#lib/welcome/steps.ts";
  import "#lib/theme/controls.css";

  let saved = $state<string | null>(null);
  let problem = $state<string | null>(null);

  // Both apply to the whole app at once, so the choice can be judged on this very screen.
  async function chooseTheme(value: string) {
    if (!isThemeChoice(value)) return;
    saved = null;
    problem = null;
    const result = await theme.set(value);
    if (result.ok) saved = "Theme saved.";
    else
      problem = `The theme changed here but Puddle couldn't save it. ${result.message}`;
  }

  async function choosePalette(value: string) {
    if (!isPaletteChoice(value)) return;
    saved = null;
    problem = null;
    const result = await palette.set(value);
    if (result.ok) saved = "Colours saved.";
    else
      problem = `The colours changed here but Puddle couldn't save them. ${result.message}`;
  }

  async function chooseDensity(value: string) {
    if (!isDensityChoice(value)) return;
    saved = null;
    problem = null;
    const result = await density.set(value);
    if (result.ok) saved = "Density saved.";
    else
      problem = `The density changed here but Puddle couldn't save it. ${result.message}`;
  }
</script>

<StepHeading
  title="Look"
  lead="Optional. Each applies at once, and you can change them any time in Settings, where you can also pick your own colours."
/>

<div class="status" aria-live="polite">
  {#if saved}<p class="saved">{saved}</p>{/if}
  {#if problem}<p class="error" role="alert">{problem}</p>{/if}
</div>

<fieldset class="card">
  <legend>Theme</legend>
  {#each THEME_OPTIONS as option (option.value)}
    <label class="choice">
      <input
        type="radio"
        name="theme"
        value={option.value}
        checked={theme.choice === option.value}
        onchange={() => void chooseTheme(option.value)}
      />
      <span>
        <b>{option.label}</b>
        <span class="desc">{option.hint}</span>
      </span>
    </label>
  {/each}
</fieldset>

<fieldset class="card">
  <legend>Colours</legend>
  {#each PALETTE_PRESET_OPTIONS as option (option.value)}
    <label class="choice">
      <input
        type="radio"
        name="palette"
        value={option.value}
        checked={palette.choice === option.value}
        onchange={() => void choosePalette(option.value)}
      />
      <span>
        <b>{option.label}</b>
        <span class="desc">{option.hint}</span>
      </span>
      <span class="swatches" aria-hidden="true">
        {#each option.swatch as [light, dark] (light + dark)}
          <span class="swatch" style:background="light-dark({light}, {dark})"
          ></span>
        {/each}
      </span>
    </label>
  {/each}
</fieldset>

<fieldset class="card">
  <legend>Density</legend>
  {#each DENSITY_OPTIONS as option (option.value)}
    <label class="choice">
      <input
        type="radio"
        name="density"
        value={option.value}
        checked={density.choice === option.value}
        onchange={() => void chooseDensity(option.value)}
      />
      <span>
        <b>{option.label}</b>
        <span class="desc">{option.hint}</span>
      </span>
    </label>
  {/each}
</fieldset>

<div class="actions">
  <a class="btn" href={around("look").back}>Back</a>
  <a class="btn primary" href={around("look").next}>Continue</a>
</div>

<style>
  .desc {
    display: block;
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .status {
    min-height: 1.5rem;
  }
  .status p {
    margin: 0;
  }
  .saved {
    color: var(--color-success);
  }
  .error {
    color: var(--color-danger);
  }
  .card {
    display: grid;
    gap: var(--space-3);
    margin: 0;
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  legend {
    padding: 0 var(--space-1);
    font-weight: 600;
  }
  .choice {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    cursor: pointer;
  }
  .choice input {
    flex: none;
    width: 1.5rem;
    height: 1.5rem;
    margin: 0;
  }
  .swatches {
    display: flex;
    margin-left: auto;
  }
  .swatch {
    width: 1.25rem;
    height: 1.25rem;
    margin-left: -0.25rem;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
