<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import ArrowDown from "@lucide/svelte/icons/arrow-down";
  import ArrowUp from "@lucide/svelte/icons/arrow-up";
  import { absoluteTime, relativeTime } from "#lib/format/relative-time.ts";
  import {
    isExpired,
    patternLabel,
    ruleName,
    scopeLabel,
    type Rule,
    type Sort,
    type SortKey,
    actorLabel,
  } from "#lib/rules/model.ts";
  import RuleRowMenu from "./RuleRowMenu.svelte";
  import "#lib/theme/controls.css";

  let {
    rules,
    sort,
    now,
    onSort,
    onExpiry,
    onDelete,
    onEdit,
    onEnabled,
  }: {
    rules: Rule[];
    sort: Sort;
    now: number;
    /** Left out, the headers are plain text. */
    onSort?: (key: SortKey) => void;
    /** Left out together with `onDelete`, the table is read-only. */
    onExpiry?: (rule: Rule, anchor: HTMLElement) => void;
    onDelete?: (rule: Rule, anchor: HTMLElement) => void;
    /** Left out, the rows have no Edit button. */
    onEdit?: (rule: Rule, anchor: HTMLElement) => void;
    /** Left out, the table has no switch column. The page changes the rule and the row follows. */
    onEnabled?: (rule: Rule, enabled: boolean) => void;
  } = $props();

  // Drawing 1000 rows in one go blocks WebKit for about 3 s. The first slice is drawn at once and the
  // rest follows one chunk per frame, so the page answers first. Every row still ends up in the DOM
  // (keyboard, screen readers and find-in-page reach all of them), and aria-rowcount gives the real
  // size meanwhile.
  const FIRST_SLICE = 50;
  const CHUNK = 50;
  let drawn = $state(FIRST_SLICE);
  const visible = $derived(rules.slice(0, drawn));
  $effect(() => {
    if (drawn >= rules.length) return;
    const frame = requestAnimationFrame(() => {
      drawn += CHUNK;
    });
    return () => cancelAnimationFrame(frame);
  });
  const editable = $derived(onExpiry !== undefined && onDelete !== undefined);

  const columns: { key: SortKey; label: string }[] = [
    { key: "pattern", label: "Host" },
    { key: "effect", label: "Effect" },
    { key: "scope", label: "Workspace" },
    { key: "expires", label: "Expires" },
    { key: "created", label: "Created" },
  ];

  const ariaSort = (key: SortKey) =>
    sort.key !== key
      ? undefined
      : sort.direction === "asc"
        ? "ascending"
        : "descending";
</script>

<div class="wrap">
  <table aria-rowcount={rules.length + 1}>
    <caption class="visually-hidden">Rules</caption>
    <thead>
      <tr>
        {#each columns as column (column.key)}
          <th
            scope="col"
            aria-sort={ariaSort(column.key)}
            class:created={column.key === "created"}
          >
            {#if onSort}
              <button
                type="button"
                class="sort"
                onclick={() => onSort(column.key)}
                aria-label="Sort by {column.label.toLowerCase()}"
              >
                {column.label}
                {#if sort.key === column.key}
                  {#if sort.direction === "asc"}
                    <ArrowUp aria-hidden="true" size={14} />
                  {:else}
                    <ArrowDown aria-hidden="true" size={14} />
                  {/if}
                {/if}
              </button>
            {:else}
              {column.label}
            {/if}
          </th>
        {/each}
        {#if onEnabled}
          <th scope="col">On</th>
        {/if}
        {#if editable}
          <th scope="col"><span class="visually-hidden">Actions</span></th>
        {/if}
      </tr>
    </thead>
    <tbody>
      {#each visible as rule, index (rule.id)}
        {@const expired = isExpired(rule, now)}
        <tr
          data-rule-id={rule.id}
          aria-rowindex={index + 2}
          class:expired
          class:off={!rule.enabled}
        >
          <td class="host">
            <span
              class="mono"
              title={rule.pattern_kind === "suffix"
                ? "This host and every subdomain of it"
                : undefined}>{patternLabel(rule)}</span
            >
            {#if !rule.enabled}<span class="meta">off</span>{/if}
          </td>
          <td>
            <span class="chip {rule.effect}"
              >{rule.effect === "allow" ? "Allow" : "Deny"}</span
            >
          </td>
          <td>
            {scopeLabel(rule)}
          </td>
          <td>
            {#if rule.expires_at === null}
              <span class="muted">Never</span>
            {:else}
              <time
                datetime={new Date(rule.expires_at).toISOString()}
                title={absoluteTime(rule.expires_at)}
                >{expired
                  ? "Expired"
                  : relativeTime(rule.expires_at, now)}</time
              >
            {/if}
          </td>
          <td class="created">
            <time
              datetime={new Date(rule.created_at).toISOString()}
              title="{absoluteTime(rule.created_at)} by {actorLabel(
                rule.created_by,
              )}">{relativeTime(rule.created_at, now)}</time
            >
          </td>
          {#if onEnabled}
            <td class="on">
              {#if !expired}
                <input
                  type="checkbox"
                  role="switch"
                  checked={rule.enabled}
                  aria-label="Rule on: {ruleName(rule)}"
                  onchange={(e) => {
                    const next = e.currentTarget.checked;
                    e.currentTarget.checked = rule.enabled;
                    onEnabled(rule, next);
                  }}
                />
              {/if}
            </td>
          {/if}
          {#if onExpiry && onDelete}
            <td class="actions">
              <RuleRowMenu {rule} {expired} {onEdit} {onExpiry} {onDelete} />
            </td>
          {/if}
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .wrap {
    /* The visually hidden "Actions" header is absolutely positioned; without a positioned
       ancestor it escapes this scroll box and makes the whole page scroll sideways. */
    position: relative;
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
  tbody tr:last-child td {
    border-bottom: 0;
  }
  .sort {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    min-height: 1.5rem;
    padding: 0 var(--space-1);
    border: 0;
    background: transparent;
    color: var(--color-text-muted);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  .host {
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--color-text-muted);
  }
  tr.off td:not(.actions):not(.on) {
    color: var(--color-text-muted);
  }
  tr.expired td:not(.actions) {
    color: var(--color-text-muted);
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    white-space: nowrap;
  }
  .chip.allow {
    color: var(--color-success);
    border-color: var(--color-success);
  }
  .chip.deny {
    color: var(--color-danger);
    border-color: var(--color-danger);
  }
  .actions {
    text-align: end;
    white-space: nowrap;
  }
  /* Below about 800 px the table must fit without a sideways scroll, since the row's menu sits at its
     right end: the Created column steps aside and the host wraps. */
  @media (max-width: 800px) {
    .created {
      display: none;
    }
    .wrap {
      overflow-x: visible;
    }
    th,
    td {
      padding-inline: var(--space-1);
    }
  }
</style>
