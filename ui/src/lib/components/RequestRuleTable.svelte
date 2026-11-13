<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { actorLabel } from "#lib/rules/model.ts";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import { absoluteTime, relativeTime } from "#lib/format/relative-time.ts";
  import {
    methodsLabel,
    requestRuleName,
    scopeLabel,
    type RequestRule,
  } from "#lib/rules/request-rules.ts";
  import "#lib/theme/controls.css";

  // Rules on method and path as a table: what each one refuses, where, and who made it. A rule
  // whose host is on a never-decrypt list carries the API's warning, shown in full under its host.
  let {
    rules,
    now,
    onDelete,
    selected,
  }: {
    rules: RequestRule[];
    now: number;
    /** Left out, the table is read-only. */
    onDelete?: ((rule: RequestRule) => void) | undefined;
    /** The rule a link named (from the Activity screen): marked, scrolled to and given focus. */
    selected?: number | undefined;
  } = $props();

  function reveal(row: HTMLElement, isSelected: boolean) {
    if (!isSelected) return;
    row.focus({ preventScroll: true });
    row.scrollIntoView({ block: "center" });
  }
</script>

<div class="wrap">
  <table>
    <caption class="visually-hidden">Rules on method and path</caption>
    <thead>
      <tr>
        <th scope="col">Host</th>
        <th scope="col">Refuses</th>
        <th scope="col">Path</th>
        <th scope="col">Workspace</th>
        <th scope="col">Made</th>
        {#if onDelete}
          <th scope="col"><span class="visually-hidden">Actions</span></th>
        {/if}
      </tr>
    </thead>
    <tbody>
      {#each rules as rule (rule.id)}
        <tr
          data-request-rule-id={rule.id}
          class:selected={rule.id === selected}
          aria-current={rule.id === selected ? "true" : undefined}
          tabindex={rule.id === selected ? -1 : undefined}
          {@attach (row) => reveal(row, rule.id === selected)}
        >
          <td class="host">
            <span class="mono">{rule.host}</span>
            {#if rule.host.startsWith("*.")}
              <span class="chip">and subdomains</span>
            {/if}
            {#if rule.warning}
              <p class="warning">
                <b>Note:</b>
                {rule.warning}
              </p>
            {/if}
          </td>
          <td class:muted={rule.methods.length === 0}>
            <span class="mono">{methodsLabel(rule)}</span>
          </td>
          <td><span class="mono">{rule.path}</span></td>
          <td>
            {#if rule.workspace === null}
              {scopeLabel(rule)}
            {:else}
              <span class="mono">{rule.workspace}</span>
            {/if}
          </td>
          <td>
            <time
              datetime={new Date(rule.created_at).toISOString()}
              title="{absoluteTime(rule.created_at)} by {actorLabel(
                rule.created_by,
              )}">{relativeTime(rule.created_at, now)}</time
            >
          </td>
          {#if onDelete}
            <td class="actions">
              <button
                type="button"
                class="btn deny"
                aria-label="Delete rule: {requestRuleName(rule)}"
                onclick={() => onDelete(rule)}
              >
                <Trash2 aria-hidden="true" size={16} />Delete
              </button>
            </td>
          {/if}
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .wrap {
    overflow-x: auto;
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  th,
  td {
    padding: var(--space-1) var(--space-3);
    text-align: start;
    border-bottom: 1px solid var(--color-border-subtle);
    vertical-align: middle;
  }
  th {
    color: var(--color-text-muted);
    font-weight: 600;
  }
  tr.selected {
    background: var(--color-accent-subtle);
    outline: 2px solid var(--color-accent);
    outline-offset: -2px;
  }
  tbody tr:last-child td {
    border-bottom: 0;
  }
  .host,
  .mono {
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--color-text-muted);
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    white-space: nowrap;
  }
  .warning {
    margin: var(--space-1) 0 0;
    max-width: 40rem;
    color: var(--color-warning);
  }
  .actions {
    text-align: end;
    white-space: nowrap;
  }
</style>
