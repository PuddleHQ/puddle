<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Tests only: pages of Settings inside the real frame (the list, the search, the saver). Several
  // pages can be shown at once, so a test reaches every setting without opening each page.
  import type { Component } from "svelte";
  import Frame from "../../routes/settings/(pages)/+layout.svelte";

  let { pages }: { pages: Component[] } = $props();
</script>

<Frame>
  {#snippet children()}
    {#each pages as Page, index (index)}
      <Page />
    {/each}
  {/snippet}
</Frame>
