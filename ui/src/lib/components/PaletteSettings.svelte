<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // The colours part of Appearance: a palette, and for Custom the colours to pick, with a warning
  // where a pick makes text, control edges or the focus ring hard to see.
  import {
    activeScheme,
    CUSTOM_COLOURS,
    describeFailure,
    failures,
    resolve,
    schemesOf,
    type Custom,
    type CustomColour,
    type PaletteChoice,
  } from "#lib/theme/palette.ts";
  import type { ThemeChoice } from "#lib/theme/theme.ts";
  import { PALETTE_INFO, PALETTE_OPTIONS } from "#lib/theme/palette-options.ts";
  import "#lib/theme/controls.css";

  let {
    choice,
    custom,
    theme,
    onchoose,
    onpick,
  }: {
    choice: PaletteChoice;
    custom: Custom;
    theme: ThemeChoice;
    onchoose: (choice: PaletteChoice) => void;
    onpick: (colour: CustomColour, value: string | null) => void;
  } = $props();

  const LABELS: Record<CustomColour, [string, string]> = {
    accent: [
      "Accent colour",
      "Buttons, links, selected items and the focus ring.",
    ],
    background: ["Background", "Behind everything."],
    surface: ["Cards and panels", "Where settings, lists and forms sit."],
    text: ["Text", "The main text; muted text is derived from it."],
  };
  /** How many lines of a long warning show before "and N more". */
  const SHOWN = 4;

  const base = $derived(resolve("custom", custom, activeScheme(theme)));
  const shown: Record<CustomColour, () => string> = {
    accent: () => base.accent,
    background: () => base.bg,
    surface: () => base.surface,
    text: () => base.text,
  };
  const problems = $derived(
    failures(choice, custom, schemesOf(theme)).map(describeFailure),
  );
</script>

<div class="setting">
  <div class="grow">
    <label for="set-palette">Colours</label>
    <p class="desc">
      {PALETTE_INFO[choice].hint}
      They apply over your light or dark theme.
    </p>
  </div>
  <select
    id="set-palette"
    value={choice}
    onchange={(e) => {
      onchoose(e.currentTarget.value as PaletteChoice);
    }}
  >
    {#each PALETTE_OPTIONS as option (option.value)}
      <option value={option.value}>{option.label}</option>
    {/each}
  </select>
</div>

{#if choice === "custom"}
  {#each CUSTOM_COLOURS as key (key)}
    <div class="setting">
      <div class="grow">
        <label for="set-colour-{key}">{LABELS[key][0]}</label>
        <p class="desc">{LABELS[key][1]}</p>
      </div>
      <input
        id="set-colour-{key}"
        type="color"
        value={shown[key]()}
        oninput={(e) => onpick(key, e.currentTarget.value)}
      />
      <button
        type="button"
        class="btn"
        aria-label="Reset {LABELS[key][0].toLowerCase()}"
        disabled={custom[key] === null}
        onclick={() => onpick(key, null)}>Reset</button
      >
    </div>
  {/each}
  {#if problems.length > 0}
    <div class="warning" role="status">
      <p>
        <b>Some of these colours are hard to read.</b> You can keep them, but they
        miss the contrast Puddle aims for (WCAG AA).
      </p>
      <ul>
        {#each problems.slice(0, SHOWN) as line (line)}
          <li>{line}</li>
        {/each}
        {#if problems.length > SHOWN}
          <li>and {problems.length - SHOWN} more.</li>
        {/if}
      </ul>
    </div>
  {/if}
{/if}

<style>
  .setting {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  label {
    font-weight: 600;
  }
  p {
    margin: 0;
  }
  .desc {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  select {
    min-height: var(--control-size);
    max-width: 18rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font: inherit;
  }
  input[type="color"] {
    width: 3rem;
    height: var(--control-size);
    padding: 0;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
  }
  .warning {
    padding: var(--space-3);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
    color: var(--color-text);
  }
  .warning ul {
    margin: var(--space-2) 0 0;
    padding-left: var(--space-4);
    font-size: var(--text-sm);
  }
  @media (max-width: 760px) {
    .setting {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
