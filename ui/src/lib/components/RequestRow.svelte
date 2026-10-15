<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import DecisionControl from "./DecisionControl.svelte";
  import { LOCAL_LABELS, type LocalCategory } from "#lib/decision/local.ts";
  import type { Choice } from "#lib/decision/model.ts";
  import { absoluteTime, relativeTime } from "#lib/format/relative-time.ts";
  import type { Row } from "#lib/stores/pending.svelte.ts";

  let {
    row,
    now,
    current = false,
    blockedBy = null,
    optionsOpen = false,
    onDecide,
    onMore,
    onFocusRow,
  }: {
    row: Row;
    /** Epoch ms the relative times are measured against; the page ticks it every 30 s. */
    now: number;
    /** The row the A, D and Enter keys act on. */
    current?: boolean;
    /** The switched-off local toggle that makes this request unapprovable (R-14). */
    blockedBy?: LocalCategory | null;
    optionsOpen?: boolean;
    onDecide: (row: Row, choice: Choice) => void;
    onMore: (row: Row, anchor: HTMLElement) => void;
    onFocusRow: (row: Row) => void;
  } = $props();

  const request = $derived(row.request);
</script>

<li
  class="req"
  class:current
  data-request-id={request.id}
  aria-current={current ? "true" : undefined}
  tabindex="-1"
  onfocusin={() => onFocusRow(row)}
>
  <p class="dest">
    <span class="host">{request.host}</span>{#if request.port === 0}<span
        class="port srv"
        title="Asked for a service record (SRV) to find where to connect; no port is known yet"
        >name lookup (SRV)</span
      >{:else}<span class="port">:{request.port}</span>{/if}
  </p>
  <p class="facts">
    Workspace <b class="mono">{request.workspace}</b> · {request.attempts}
    {request.attempts === 1 ? "attempt" : "attempts"} · First seen
    <time
      datetime={new Date(request.first_seen).toISOString()}
      title={absoluteTime(request.first_seen)}
      >{relativeTime(request.first_seen, now)}</time
    >
    · Last seen
    <time
      datetime={new Date(request.last_seen).toISOString()}
      title={absoluteTime(request.last_seen)}
      >{relativeTime(request.last_seen, now)}</time
    >
  </p>
  <DecisionControl
    target={{ host: request.host, registrableDomain: row.domain }}
    workspace={request.workspace}
    {optionsOpen}
    denyOnly={blockedBy !== null}
    onDecide={(choice) => onDecide(row, choice)}
    onMore={(anchor) => onMore(row, anchor)}
  />
  {#if blockedBy}
    <p class="blocked">
      <b>{LOCAL_LABELS[blockedBy]}</b> destinations are switched off, so this
      can't be approved yet. Turn on
      <a href="/settings#local-destinations">{LOCAL_LABELS[blockedBy]}</a>
      in Settings to make it approvable; it will still need your approval after that.
    </p>
  {/if}
</li>

<style>
  .req {
    display: grid;
    grid-template-columns: 1fr auto;
    column-gap: var(--space-4);
    align-items: center;
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--color-border-subtle);
    border-inline-start: 3px solid transparent;
  }
  .req:last-child {
    border-bottom: 0;
  }
  .req.current {
    border-inline-start-color: var(--color-accent);
    background: var(--color-accent-subtle);
  }
  .req:focus {
    outline: none;
  }
  .req:focus-visible {
    outline: var(--focus-ring);
    outline-offset: -2px;
  }
  .dest,
  .facts {
    grid-column: 1;
    min-width: 0;
  }
  .req > :global(.control) {
    grid-column: 2;
    grid-row: 1 / span 2;
  }
  p {
    margin: 0;
  }
  .dest {
    overflow-wrap: anywhere;
  }
  .host {
    font-family: var(--font-mono);
    font-weight: 600;
  }
  .port {
    font-family: var(--font-mono);
    color: var(--color-text-muted);
  }
  .port.srv {
    margin-inline-start: var(--space-2);
    font-family: inherit;
    font-size: var(--text-sm);
  }
  .facts {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .blocked {
    grid-column: 1 / -1;
    margin-top: var(--space-2);
    padding: var(--space-2) var(--space-3);
    background: var(--color-surface-raised);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
  }
  @media (max-width: 760px) {
    .req {
      grid-template-columns: 1fr;
    }
    .req > :global(.control) {
      grid-column: 1;
      grid-row: auto;
      margin-top: var(--space-2);
    }
  }
</style>
