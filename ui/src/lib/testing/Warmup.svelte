<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // Renders once in every test file's setup (see vitest.setup.ts): the constructs the pages use,
  // so the first real test does not pay for Svelte's and jsdom's first use of each.
  let count = $state(0);
  let text = $state("");
  const items = $derived(Array.from({ length: count }, (_, i) => i));
</script>

<h1>Warm-up</h1>
<label>Text<input type="text" bind:value={text} /></label>
<button type="button" onclick={() => (count += 1)}>More</button>
{#if count > 0}
  <ul>
    {#each items as item (item)}<li>{item}{text}</li>{/each}
  </ul>
{/if}
