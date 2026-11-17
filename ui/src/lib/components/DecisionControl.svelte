<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import {
    everywhere,
    narrowest,
    type Choice,
    type Target,
  } from "#lib/decision/model.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import "#lib/theme/controls.css";

  // Ctrl+click (Cmd+click on a Mac) decides for every workspace and skips the options.
  const choose = (effect: "allow" | "deny", event: MouseEvent) =>
    onDecide(
      event.ctrlKey || event.metaKey ? everywhere(effect) : narrowest(effect),
    );

  // Allow and Deny decide with one click: this workspace, the exact host, permanently. The grey
  // button after them belongs to neither: its options (every workspace, subdomains, duration, a
  // rule set) end in an Allow and a Deny of their own (OptionsPopover). The choice is a plain
  // `Choice`; what happens with it (confirm, API call) is the parent's.
  let {
    target,
    workspace,
    denyOnly = false,
    optionsOpen = false,
    onDecide,
    onMore,
  }: {
    target: Target;
    workspace: string;
    /** The destination can't be approved yet; only Deny is offered. */
    denyOnly?: boolean;
    /** Whether the options are open for this row, for the more-choices button's `aria-expanded`. */
    optionsOpen?: boolean;
    onDecide: (choice: Choice) => void;
    /** The more-choices button was used; the element is where the options hang from. */
    onMore: (anchor: HTMLElement) => void;
  } = $props();

  // `workspace` is the id; a person hears the name.
  const spoken = $derived(workspaces.spokenLabelOf(workspace));
</script>

<div class="control">
  {#if !denyOnly}
    <button
      type="button"
      class="btn allow"
      aria-label="Allow {target.host} for {spoken}"
      title="Allow for {spoken}, this host only, permanently (A). Ctrl+click: every workspace"
      onclick={(event) => choose("allow", event)}
    >
      Allow
    </button>
  {/if}
  <button
    type="button"
    class="btn deny"
    aria-label="Deny {target.host} for {spoken}"
    title="Deny for {spoken}, this host only, permanently (D). Ctrl+click: every workspace"
    onclick={(event) => choose("deny", event)}
  >
    Deny
  </button>
  {#if !denyOnly}
    <button
      type="button"
      class="btn more"
      data-more
      aria-label="More choices for {target.host}"
      aria-haspopup="dialog"
      aria-expanded={optionsOpen}
      title="More choices: every workspace, subdomains, how long (Enter)"
      onclick={(event) => onMore(event.currentTarget)}
    ></button>
  {/if}
</div>

<style>
  .control {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    justify-content: flex-end;
  }
  .more {
    color: var(--color-text-muted);
    padding-inline: var(--space-2);
  }
  /* The arrow is drawn here, not as an icon element: an inbox of 500 rows draws 500 of them. */
  .more::after {
    content: "";
    width: 0.5rem;
    height: 0.5rem;
    border-right: 2px solid currentColor;
    border-bottom: 2px solid currentColor;
    transform: translateY(-25%) rotate(45deg);
  }
</style>
