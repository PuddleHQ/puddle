<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
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
  import SettingRow from "./SettingRow.svelte";
  import "#lib/theme/controls.css";

  let {
    choice,
    custom,
    theme,
    onchoose,
    onpreview,
    onpick,
  }: {
    choice: PaletteChoice;
    custom: Custom;
    theme: ThemeChoice;
    onchoose: (choice: PaletteChoice) => void;
    /** A colour is being dragged: show it, store nothing yet. */
    onpreview: (colour: CustomColour, value: string) => void;
    /** A colour was settled (picker closed, or reset with `null`): store it. */
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

<SettingRow
  id="set-palette"
  label="Colours"
  desc="{PALETTE_INFO[choice].hint} They apply over your light or dark theme."
>
  <select
    id="set-palette"
    aria-describedby="set-palette-desc"
    value={choice}
    onchange={(e) => {
      onchoose(e.currentTarget.value as PaletteChoice);
    }}
  >
    {#each PALETTE_OPTIONS as option (option.value)}
      <option value={option.value}>{option.label}</option>
    {/each}
  </select>
</SettingRow>

{#if choice === "custom"}
  {#each CUSTOM_COLOURS as key (key)}
    <SettingRow
      id="set-colour-{key}"
      label={LABELS[key][0]}
      desc={LABELS[key][1]}
    >
      <div class="pick">
        <input
          id="set-colour-{key}"
          type="color"
          aria-describedby="set-colour-{key}-desc"
          value={shown[key]()}
          oninput={(e) => onpreview(key, e.currentTarget.value)}
          onchange={(e) => onpick(key, e.currentTarget.value)}
        />
        <button
          type="button"
          class="btn"
          aria-label="Reset {LABELS[key][0].toLowerCase()}"
          disabled={custom[key] === null}
          onclick={() => onpick(key, null)}
        >
          Reset
        </button>
      </div>
    </SettingRow>
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
  .pick {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  p {
    margin: 0;
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
</style>
