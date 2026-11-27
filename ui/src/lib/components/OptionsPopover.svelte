<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { Popover } from "bits-ui";
  import {
    DURATIONS,
    suffixFor,
    type Choice,
    type Effect,
    type Match,
    type RuleSetChoice,
    type Target,
  } from "#lib/decision/model.ts";
  import AllowDenyPair from "./AllowDenyPair.svelte";
  import SetChip from "./SetChip.svelte";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import "#lib/theme/controls.css";

  /** Under this window width the buttons sit under the host and the box opens below them. */
  const BESIDE_MIN_WIDTH = 900;

  // The "more choices": every workspace, subdomains, duration, then Allow or Deny with them. One
  // instance serves the whole list, anchored to the more-choices button of the row it was opened
  // from (500 rows, one popover). That button is neither Allow nor Deny, so both end it.
  let {
    open = $bindable(false),
    target,
    workspace,
    anchor,
    exactOnly = false,
    sets = [],
    onDecide,
    onClose,
  }: {
    open?: boolean;
    target: Target;
    workspace: string;
    /** The more-choices button it hangs from, and gives focus back to. */
    anchor: HTMLElement | null;
    /** Local destinations: only an exact rule counts (R-14), so no subdomain choice. */
    exactOnly?: boolean;
    /** Your rule sets that are on for this workspace: the rule can go into one of them. */
    sets?: readonly RuleSetChoice[];
    onDecide: (choice: Choice) => void;
    onClose: () => void;
  } = $props();

  // "workspace", "global", or "set:<id>".
  let who = $state("workspace");
  let match = $state<Match>("exact");
  let duration = $state("0");
  /** Set when the chosen rule set was deleted while the popover was open. */
  let stale = $state<string | null>(null);

  const suffix = $derived(exactOnly ? null : suffixFor(target));
  const durationSecs = $derived(duration === "0" ? null : Number(duration));
  const id = $props.id();

  // The whole button group of the row, not just the arrow, so the box clears Allow and Deny too.
  const place = $derived(anchor?.closest<HTMLElement>(".control") ?? anchor);
  let innerWidth = $state(1280);
  const sideBeside = $derived(innerWidth >= BESIDE_MIN_WIDTH);

  $effect(() => {
    // Every time it opens it starts from the narrowest choice (R-15).
    if (open) {
      who = "workspace";
      match = "exact";
      duration = "0";
      stale = null;
    }
  });

  function decide(effect: Effect) {
    const ruleSet = sets.find((s) => `set:${s.id}` === who) ?? null;
    if (ruleSet === null && who.startsWith("set:")) {
      stale = "That rule set no longer exists. Choose again.";
      return;
    }
    stale = null;
    const scope = who === "global" ? "global" : "workspace";
    onDecide({ effect, scope, ruleSet, match, durationSecs });
  }
</script>

<svelte:window bind:innerWidth />

<Popover.Root
  bind:open
  onOpenChange={(next) => {
    if (!next) onClose();
  }}
>
  <Popover.Portal>
    <!-- Wide window: it opens to the left of the row's Allow, Deny and more-choices buttons, so every
         row's buttons stay visible and clickable and only host names are covered. Narrow window (the
         buttons sit under the host, no room beside them): it opens below them. -->
    <Popover.Content
      class="options"
      role="dialog"
      side={sideBeside ? "left" : "bottom"}
      align={sideBeside ? "center" : "end"}
      sideOffset={8}
      customAnchor={place}
      aria-label="Choices for {target.host}"
      onInteractOutside={(event) => {
        // A click on the more-choices button toggles; it must not close here and reopen there.
        if (event.target instanceof Node && anchor?.contains(event.target)) {
          event.preventDefault();
        }
      }}
      onCloseAutoFocus={(event) => {
        event.preventDefault();
        // bits-ui also runs this while the popover stays open: opened again, it sets up its focus
        // trap a second time once the opening transition ends, a frame later. Focus goes back to
        // the more-choices button only on a real close; otherwise a choice focused in that frame is lost.
        if (!open && anchor?.isConnected) anchor.focus();
      }}
    >
      <fieldset>
        <legend>Who</legend>
        <label>
          <input
            type="radio"
            name="{id}-scope"
            value="workspace"
            bind:group={who}
          />
          <span class="text">
            Only <b>{workspaces.spokenLabelOf(workspace)}</b>
          </span>
        </label>
        <label>
          <input
            type="radio"
            name="{id}-scope"
            value="global"
            bind:group={who}
          />
          <span class="text">
            Every workspace <span class="hint">(asks to confirm)</span>
          </span>
        </label>
        {#each sets as set (set.id)}
          <label>
            <input
              type="radio"
              name="{id}-scope"
              value="set:{set.id}"
              bind:group={who}
            />
            <span class="text">
              Into rule set
              <b><SetChip name={set.name} fill={set.fill} /></b>
              {#if set.everywhere}<span class="hint">
                  (on everywhere; asks)
                </span>{/if}
            </span>
          </label>
        {/each}
      </fieldset>
      <fieldset>
        <legend>Which hosts</legend>
        <label>
          <input
            type="radio"
            name="{id}-match"
            value="exact"
            bind:group={match}
          />
          <span class="text">Only <span class="mono">{target.host}</span></span>
        </label>
        {#if suffix}
          <label>
            <input
              type="radio"
              name="{id}-match"
              value="suffix"
              bind:group={match}
            />
            <span class="text">
              Everything under
              <span class="mono">{suffix.slice(1)}</span>
            </span>
          </label>
        {/if}
      </fieldset>
      <div class="field">
        <label for="{id}-duration">How long</label>
        <select id="{id}-duration" bind:value={duration}>
          {#each DURATIONS as option (option.secs)}
            <option value={String(option.secs ?? 0)}>{option.label}</option>
          {/each}
        </select>
      </div>
      {#if stale}<p class="error" role="alert">{stale}</p>{/if}
      <div class="actions">
        <Popover.Close class="btn">Cancel</Popover.Close>
        <AllowDenyPair
          icons
          onAllow={() => decide("allow")}
          onDeny={() => decide("deny")}
        />
      </div>
    </Popover.Content>
  </Popover.Portal>
</Popover.Root>

<style>
  :global(.options) {
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
  :global(.options fieldset) {
    display: grid;
    gap: var(--space-1);
    margin: 0;
    padding: 0;
    border: 0;
  }
  :global(.options legend) {
    padding: 0;
    margin-bottom: var(--space-1);
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--color-text-muted);
  }
  :global(.options label) {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-height: 1.5rem;
  }
  /* One flex item per choice: words stay whole, only a host name may break. */
  :global(.options label .text) {
    min-width: 0;
    overflow-wrap: normal;
  }
  :global(.options label .mono) {
    overflow-wrap: anywhere;
  }
  :global(.options .field) {
    display: grid;
    gap: var(--space-1);
  }
  :global(.options .field label) {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--color-text-muted);
  }

  :global(.options .hint) {
    color: var(--color-text-muted);
  }
  :global(.options .actions) {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
