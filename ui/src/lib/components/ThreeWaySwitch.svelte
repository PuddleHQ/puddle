<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import "#lib/theme/controls.css";

  // Follow, On or Off for one thing: the highlighted segment is the current state, so nothing has to
  // be read from beside it. `inherited` is what Follow means right now (the every-workspace value).
  let {
    label,
    value,
    inherited,
    onChange,
  }: {
    label: string;
    /** `null` = follow, otherwise the thing's own setting. */
    value: boolean | null;
    inherited: boolean;
    onChange: (next: boolean | null) => void;
  } = $props();

  const segments = $derived<{ text: string; next: boolean | null }[]>([
    { text: `Follow (${inherited ? "on" : "off"})`, next: null },
    { text: "On", next: true },
    { text: "Off", next: false },
  ]);
</script>

<span class="three" role="group" aria-label={label}>
  {#each segments as segment (segment.text)}
    <button
      type="button"
      class="btn segment"
      aria-pressed={segment.next === value}
      onclick={() => {
        if (segment.next !== value) onChange(segment.next);
      }}
    >
      {segment.text}
    </button>
  {/each}
</span>

<style>
  .three {
    display: inline-flex;
  }
  .segment {
    border-radius: 0;
  }
  .segment:first-child {
    border-start-start-radius: var(--radius-md);
    border-end-start-radius: var(--radius-md);
  }
  .segment:last-child {
    border-start-end-radius: var(--radius-md);
    border-end-end-radius: var(--radius-md);
  }
  .segment + .segment {
    margin-inline-start: -1px;
  }
  .segment[aria-pressed="true"] {
    background: var(--color-accent);
    border-color: var(--color-accent);
    color: var(--color-accent-contrast);
  }
</style>
