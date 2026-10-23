<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // Puddle's default mark: a rubber duck on a patch of water. It is drawn from the theme's duck
  // tokens, so a theme that recolours them recolours the mark. Decorative by default (the name
  // beside it says "Puddle"); pass `label` where it stands alone.
  let {
    size = 24,
    water = true,
    label,
  }: { size?: number; water?: boolean; label?: string } = $props();
</script>

<svg
  class="duck"
  viewBox="0 0 64 64"
  width={size}
  height={size}
  role={label ? "img" : undefined}
  aria-label={label}
  aria-hidden={label ? undefined : "true"}
  focusable="false"
>
  {#if water}
    <ellipse cx="32" cy="55" rx="27" ry="5" class="water" />
  {/if}
  <path
    class="body"
    d="M9 30c3 1 5 4 9 5 5 1 12-1 18-1 8 0 17 2 18 10 1 7-6 12-20 12-14 0-25-4-25-14 0-4 0-8 0-12z"
  />
  <path class="wing" d="M18 41c5-4 14-4 18 2-5 4-13 4-18-2z" />
  <circle class="body" cx="40" cy="22" r="11" />
  <path class="beak" d="M49.5 21.5c4-.6 8 .2 10 1.6-2 2.4-6 3.6-10 3.2z" />
  <circle class="eye" cx="43" cy="19" r="2" />
  <circle class="glint" cx="43.6" cy="18.4" r=".6" />
</svg>

<style>
  .duck {
    flex: none;
    display: block;
  }
  .water {
    fill: var(--color-water);
  }
  .body {
    fill: var(--color-duck);
  }
  .wing {
    fill: var(--color-duck-ink);
    fill-opacity: 0.12;
  }
  .beak {
    fill: var(--color-beak);
  }
  .eye {
    fill: var(--color-duck-ink);
  }
  .glint {
    fill: var(--color-duck);
  }
</style>
