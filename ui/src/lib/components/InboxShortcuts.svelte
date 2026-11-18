<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { Popover } from "bits-ui";
  import Keyboard from "@lucide/svelte/icons/keyboard";
  import { INBOX_SHORTCUTS } from "#lib/decision/inbox-shortcuts.ts";
  import "#lib/theme/controls.css";

  // A small button beside the inbox heading; its popover lists the shortcuts from the one table
  // the tests check. The single letters follow the Settings switch, so they are listed only
  // while they work.
  let {
    singleKeys,
  }: {
    /** Whether the Settings switch for the inbox's single keys is on. */
    singleKeys: boolean;
  } = $props();

  const shown = $derived(
    INBOX_SHORTCUTS.filter((s) => singleKeys || !s.singleKey),
  );
</script>

<Popover.Root>
  <Popover.Trigger
    class="btn shortcuts-trigger"
    aria-label="Keyboard shortcuts"
    title="Keyboard shortcuts"
  >
    <Keyboard aria-hidden="true" size={18} />
  </Popover.Trigger>
  <Popover.Portal>
    <Popover.Content
      class="shortcuts"
      role="dialog"
      aria-label="Keyboard shortcuts"
      align="end"
      sideOffset={6}
    >
      <dl>
        {#each shown as shortcut (shortcut.id)}
          <div class="line" data-shortcut={shortcut.id}>
            <dt>
              {#each shortcut.keys as key, i (key)}{#if i > 0}+{/if}<kbd
                  class="kbd">{key}</kbd
                >{/each}
            </dt>
            <dd>{shortcut.does}</dd>
          </div>
        {/each}
      </dl>
      <p class="note">
        {#if singleKeys}
          A, D, J and K act on the marked request.
          <a class="link-block" href="/settings/notifications#set-confirm-global"
            >Turn the single keys off in Settings</a
          >
        {:else}
          The single keys A, D, J and K are off.
          <a class="link-block" href="/settings/notifications#set-confirm-global"
            >Turn them on in Settings</a
          >
        {/if}
      </p>
    </Popover.Content>
  </Popover.Portal>
</Popover.Root>

<style>
  :global(.shortcuts-trigger) {
    color: var(--color-text-muted);
    padding-inline: var(--space-2);
  }
  :global(.shortcuts) {
    z-index: 30;
    width: min(22rem, calc(100vw - 2 * var(--space-4)));
    padding: var(--space-4);
    display: grid;
    gap: var(--space-3);
    background: var(--color-surface);
    color: var(--color-text);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
  }
  :global(.shortcuts dl) {
    display: grid;
    gap: var(--space-2);
    margin: 0;
  }
  :global(.shortcuts .line) {
    display: grid;
    grid-template-columns: 7rem 1fr;
    gap: var(--space-2);
    align-items: baseline;
  }
  :global(.shortcuts dt),
  :global(.shortcuts dd) {
    margin: 0;
  }
  :global(.shortcuts .note) {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
</style>
