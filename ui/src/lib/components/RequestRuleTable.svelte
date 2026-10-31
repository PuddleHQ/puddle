<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
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
  }: {
    rules: RequestRule[];
    now: number;
    /** Left out, the table is read-only. */
    onDelete?: ((rule: RequestRule) => void) | undefined;
  } = $props();
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
        <tr data-request-rule-id={rule.id}>
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
              title="{absoluteTime(rule.created_at)} by {rule.created_by}"
              >{relativeTime(rule.created_at, now)}</time
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
