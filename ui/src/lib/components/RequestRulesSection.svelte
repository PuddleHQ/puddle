<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import Plus from "@lucide/svelte/icons/plus";
  import { settleFocus } from "#lib/focus.ts";
  import {
    REQUEST_RULES_INTRO,
    REQUEST_RULES_NOTE,
    methodsLabel,
    requestRuleName,
    requestRulesView,
    type NewRequestRule,
    type RequestRule,
  } from "#lib/rules/request-rules.ts";
  import {
    requestRulesStore,
    type RequestRuleRefusal,
    type RequestRulesStore,
  } from "#lib/stores/request-rules.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import AddRequestRuleDialog from "./AddRequestRuleDialog.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import LoadNotice from "./LoadNotice.svelte";
  import RequestRuleTable from "./RequestRuleTable.svelte";
  import "#lib/theme/controls.css";

  // Rules on the method and path of a request. On the Rules screen it lists, adds and deletes them.
  // With a `workspace` it is the read-only list of the rules that apply to that workspace, for its
  // Network tab, and sends the user to the Rules screen to change them.
  let {
    store = requestRulesStore,
    workspace,
    workspaces = [],
    now,
  }: {
    store?: RequestRulesStore;
    workspace?: string;
    /** Workspaces to suggest in the form. */
    workspaces?: string[];
    now: number;
  } = $props();

  const id = $props.id();
  const editable = $derived(workspace === undefined);
  const shown = $derived(requestRulesView(store.rules, workspace));
  let heading = $state<HTMLElement>();
  let addOpen = $state(false);
  let addError = $state<RequestRuleRefusal | null>(null);
  let addDialog = $state<{ clear: () => void }>();
  let draft = $state<NewRequestRule | null>(null);
  let confirmGlobalOpen = $state(false);
  let deleting = $state<RequestRule | null>(null);
  let deleteOpen = $state(false);

  onMount(() => {
    // The Rules screen and the Network tab both own the store's life; a second start is harmless.
    return store.start();
  });

  const draftSummary = $derived(
    draft
      ? `Refuse ${draft.methods?.length ? draft.methods.join(", ") : "every method"} ${draft.path} on ${draft.host} for every workspace`
      : "",
  );

  async function create(
    rule: NewRequestRule,
  ): Promise<RequestRuleRefusal | null> {
    if (rule.workspace === null) {
      draft = rule;
      addOpen = false;
      confirmGlobalOpen = true;
      return null;
    }
    return finishAdd(rule);
  }

  async function finishAdd(
    rule: NewRequestRule,
  ): Promise<RequestRuleRefusal | null> {
    const result = await store.add(rule);
    if (!result.ok) return { field: result.field, message: result.message };
    addOpen = false;
    addDialog?.clear();
    toasts.push(`Added: ${requestRuleName(result.rule)}.`);
    return null;
  }

  async function confirmGlobal(rule: NewRequestRule) {
    draft = null;
    const refusal = await finishAdd(rule);
    if (refusal) {
      addError = refusal;
      addOpen = true;
    }
  }

  function cancelGlobal() {
    draft = null;
    addOpen = true;
  }

  async function confirmDelete(rule: RequestRule) {
    deleting = null;
    const index = shown.findIndex((r) => r.id === rule.id);
    const neighbours = [shown[index + 1], shown[index - 1]]
      .filter((r): r is RequestRule => r !== undefined)
      .map((r) => `[data-request-rule-id="${r.id}"] button`);
    const result = await store.remove(rule.id);
    toasts.push(
      result.ok ? `Deleted: ${requestRuleName(rule)}.` : result.message,
      result.ok ? {} : { tone: "error", ms: 8000 },
    );
    await settleFocus(neighbours, heading);
  }
</script>

<section
  aria-labelledby="{id}-title"
  class={editable ? "section" : "block"}
  data-testid="request-rules"
>
  <div class="head">
    <div>
      <h2 id="{id}-title" tabindex="-1" bind:this={heading}>
        {editable ? "Method and path" : "Rules on method and path"}
      </h2>
      {#if editable}
        <p class="sub">{REQUEST_RULES_INTRO}</p>
        <p class="sub">{REQUEST_RULES_NOTE}</p>
      {/if}
    </div>
    {#if editable}
      <button
        type="button"
        class="btn"
        onclick={() => {
          addDialog?.clear();
          addOpen = true;
        }}><Plus aria-hidden="true" size={16} />Add method and path rule</button
      >
    {:else}
      <a class="btn" href="/rules">All method and path rules</a>
    {/if}
  </div>
  {#if store.status === "ready" && store.loadError}
    <LoadNotice
      what="the rules on method and path"
      message={store.loadError}
      stale
      padded={!editable}
    />
  {/if}
  {#if store.status === "loading"}
    <p class="muted" class:pad={!editable}>
      Loading the method and path rules&hellip;
    </p>
  {:else if store.status === "failed"}
    <LoadNotice
      what="the rules on method and path"
      message={store.loadError}
      padded={!editable}
    />
  {:else if shown.length === 0}
    <p class="muted" class:pad={!editable}>
      {#if editable}
        No rules on method and path yet. A rule here refuses, for example,
        <span class="mono">DELETE</span> on <span class="mono">/repos/**</span> at
        a host that a workspace may otherwise reach.
      {:else}
        No rule on method and path applies to {workspace}.
      {/if}
    </p>
  {:else}
    <RequestRuleTable
      rules={shown}
      {now}
      onDelete={editable
        ? (rule) => {
            deleting = rule;
            deleteOpen = true;
          }
        : undefined}
    />
    {#if !editable}
      <p class="muted note">
        Change or delete a rule on the <a href="/rules">Rules page</a>.
      </p>
    {/if}
  {/if}
</section>

{#if editable}
  <AddRequestRuleDialog
    bind:this={addDialog}
    bind:open={addOpen}
    bind:error={addError}
    {workspaces}
    onSubmit={create}
  />
  <ConfirmDialog
    bind:open={deleteOpen}
    title="Delete this rule?"
    summary={deleting
      ? `${requestRuleName(deleting)} (${methodsLabel(deleting)})`
      : ""}
    detail="The requests it refused are decided by your other rules again from the next connection a workspace opens. The host goes back to Puddle's decrypt setting. Connections that are already open keep the rules they began with."
    confirmLabel="Delete rule"
    tone="deny"
    onConfirm={() => {
      if (deleting) void confirmDelete(deleting);
    }}
    onCancel={() => {
      deleting = null;
    }}
  />
  <ConfirmDialog
    bind:open={confirmGlobalOpen}
    title="Refuse in every workspace?"
    summary={draftSummary}
    detail="This covers every workspace you have now and any you create later, and Puddle decrypts the host for all of them. You can delete the rule here."
    confirmLabel="Refuse in every workspace"
    tone="deny"
    onConfirm={() => {
      if (draft) void confirmGlobal(draft);
    }}
    onCancel={cancelGlobal}
  />
{/if}

<style>
  .section {
    display: grid;
    gap: var(--space-3);
    margin-top: var(--space-6);
  }
  .block {
    margin-bottom: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: var(--space-4);
  }
  .block .head {
    align-items: center;
    padding-inline-end: var(--space-4);
  }
  h2 {
    margin: 0;
    font-size: var(--text-lg);
  }
  h2:focus {
    outline: none;
  }
  .block h2 {
    padding: var(--space-3) var(--space-4);
    font-size: var(--text-md);
  }
  .block .head {
    border-bottom: 1px solid var(--color-border-subtle);
  }
  .block .head > div {
    flex: 1;
  }
  .sub,
  .muted {
    margin: var(--space-1) 0 0;
    color: var(--color-text-muted);
    max-width: 52rem;
  }
  .pad {
    padding: var(--space-4);
  }
  .note {
    padding: var(--space-2) var(--space-4) var(--space-3);
    font-size: var(--text-sm);
  }
</style>
