<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // `/settings` is no page of its own: it opens the first one, or the page an old `#anchor` link
  // (from a notice, the inbox or a bookmark) now lives on.
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { legacyTarget } from "#lib/settings/pages.ts";

  onMount(() => {
    void goto(legacyTarget(window.location.hash), { replaceState: true });
  });
</script>

<p class="muted">Opening settings&hellip;</p>

<style>
  .muted {
    color: var(--color-text-muted);
  }
</style>
