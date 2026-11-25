<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { absoluteTime } from "#lib/format/relative-time.ts";
  import {
    EXPIRY_OPTIONS,
    PRECEDENCES,
    PRECEDENCE_FIELD_SHOWN,
    expiryFrom,
    patternLabel,
    workspaceNameError,
    type Rule,
  } from "#lib/rules/model.ts";
  import type { RuleSetChoice } from "#lib/decision/model.ts";
  import type { NewRule, ServerError } from "#lib/stores/rules.svelte.ts";
  import FormDialog from "./FormDialog.svelte";
  import SetChip from "./SetChip.svelte";
  import WorkspaceSelect from "./WorkspaceSelect.svelte";

  // The form keeps what was typed while it is closed, so a refused rule (or a cancelled confirm)
  // can be corrected instead of retyped. `onSubmit` gets a complete request; it returns `null` when
  // it has taken over (the confirm step) and the refusal otherwise. `edit(rule)` fills the form
  // from a rule and turns it into the edit form: the page then saves over that rule.
  let {
    open = $bindable(false),
    workspaces,
    sets = [],
    now,
    error = $bindable(null),
    showPrecedence = PRECEDENCE_FIELD_SHOWN,
    onSubmit,
  }: {
    open?: boolean;
    /** Workspaces the rules already name, offered as suggestions. */
    workspaces: string[];
    /** Your rule sets: the rule can go into one of them instead. */
    sets?: readonly RuleSetChoice[];
    now: () => number;
    error?: ServerError | null;
    /** Offer the precedence field; hidden in the v0.1 build (see `PRECEDENCE_FIELD_SHOWN`). */
    showPrecedence?: boolean;
    onSubmit: (rule: NewRule) => Promise<ServerError | null>;
  } = $props();

  const id = $props.id();
  let pattern = $state("");
  let effect = $state<"allow" | "deny">("allow");
  // "workspace", "global", or "set:<id>".
  let scope = $state("workspace");
  let workspace = $state("");
  // "0" never, "keep" the expiry the rule being edited has, else seconds from now.
  let duration = $state("0");
  // The rule being edited, or `null` when the form adds one.
  let editing = $state<Rule | null>(null);
  // "" leaves it to the server: the rule set's default, else 5.
  let precedence = $state("");
  let busy = $state(false);
  let patternProblem = $state<string | null>(null);
  let workspaceProblem = $state<string | null>(null);

  const patternMessage = $derived(
    patternProblem ?? (error?.field === "pattern" ? error.message : null),
  );
  const formMessage = $derived(error?.field === "form" ? error.message : null);

  function reset() {
    editing = null;
    pattern = "";
    effect = "allow";
    scope = "workspace";
    workspace = "";
    duration = "0";
    precedence = "";
    patternProblem = null;
    workspaceProblem = null;
    error = null;
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    error = null;
    patternProblem =
      pattern.trim() === ""
        ? "Enter a host, such as example.com or *.example.com."
        : null;
    workspaceProblem =
      scope === "workspace" ? workspaceNameError(workspace.trim()) : null;
    if (patternProblem || workspaceProblem) return;
    const expires =
      duration === "keep" && editing
        ? editing.expires_at
        : expiryFrom(now(), duration === "0" ? null : Number(duration));
    const chosen = scopeOf(scope);
    if (chosen === null) {
      error = {
        field: "form",
        message:
          "That rule set no longer exists. Choose where the rule applies again.",
      };
      return;
    }
    busy = true;
    try {
      const refusal = await onSubmit({
        effect,
        pattern: pattern.trim(),
        scope: chosen,
        expires_at: expires,
        ...(precedence === "" ? {} : { precedence: Number(precedence) }),
      });
      if (refusal) error = refusal;
    } finally {
      busy = false;
    }
  }

  /** The scope the form chose, or `null` when it names a rule set that no longer exists. */
  function scopeOf(choice: string): NewRule["scope"] | null {
    const set = sets.find((s) => `set:${s.id}` === choice);
    if (set) return { type: "set", set: set.id };
    if (choice.startsWith("set:")) return null;
    if (choice === "global") return { type: "global" };
    return { type: "workspace", workspace: workspace.trim() as never };
  }

  /** Fills the form from `rule` and opens it as the edit form; the page calls it from a row. */
  export function edit(rule: Rule): void {
    reset();
    editing = rule;
    pattern = patternLabel(rule);
    effect = rule.effect;
    duration = "keep";
    precedence = showPrecedence ? String(rule.precedence) : "";
    if (rule.scope.type === "set") {
      scope = `set:${rule.scope.set}`;
    } else if (rule.scope.type === "global") {
      scope = "global";
    } else {
      scope = "workspace";
      workspace = rule.scope.workspace;
    }
    open = true;
  }

  /** Empties the form and puts the rule into this set; the page calls it from a set's card. */
  export function intoSet(set: number): void {
    reset();
    scope = `set:${set}`;
  }

  /** Empties the form and fills a host (and the workspace it is for); Activity's "Make a rule from this". */
  export function prefill(host: string, workspaceId: string | null): void {
    reset();
    pattern = host;
    if (workspaceId !== null) workspace = workspaceId;
  }

  /** Empties the form; the page calls it after a rule was added. */
  export function clear(): void {
    reset();
  }
</script>

<FormDialog
  bind:open
  title={editing ? "Edit rule" : "Add a rule"}
  description={editing
    ? "Change what this rule decides. The change applies to the next request and closes any waiting request the rule now decides."
    : "Rules decide what a workspace may reach. A new rule applies to the next request and closes any waiting request it decides."}
>
  <form onsubmit={submit} novalidate>
    <div class="field">
      <label for="{id}-pattern">Host</label>
      <input
        id="{id}-pattern"
        type="text"
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        placeholder="example.com or *.example.com"
        bind:value={pattern}
        aria-invalid={patternMessage ? "true" : undefined}
        aria-describedby="{id}-pattern-hint{patternMessage
          ? ` ${id}-pattern-error`
          : ''}"
      />
      <p class="hint" id="{id}-pattern-hint">
        <span class="mono">*.example.com</span> covers every name under
        example.com, not example.com itself. A whole top-level domain such as
        <span class="mono">*.com</span> is refused.
      </p>
      {#if patternMessage}
        <p class="error" id="{id}-pattern-error" role="alert">
          {patternMessage}
        </p>
      {/if}
    </div>
    <fieldset>
      <legend>Effect</legend>
      <label class="choice">
        <input
          type="radio"
          name="{id}-effect"
          value="allow"
          bind:group={effect}
        />
        Allow
      </label>
      <label class="choice">
        <input
          type="radio"
          name="{id}-effect"
          value="deny"
          bind:group={effect}
        />
        Deny
      </label>
    </fieldset>
    <fieldset>
      <legend>Who</legend>
      <label class="choice">
        <input
          type="radio"
          name="{id}-scope"
          value="workspace"
          bind:group={scope}
        />
        One workspace
      </label>
      <label class="choice">
        <input
          type="radio"
          name="{id}-scope"
          value="global"
          bind:group={scope}
        />
        Every workspace <span class="hint">(asks to confirm)</span>
      </label>
      {#each sets as set (set.id)}
        <label class="choice">
          <input
            type="radio"
            name="{id}-scope"
            value="set:{set.id}"
            bind:group={scope}
          />
          In rule set <b><SetChip name={set.name} fill={set.fill} /></b>
          {#if set.everywhere}<span class="hint">(on everywhere; asks)</span
            >{/if}
        </label>
      {/each}
      {#if scope === "workspace"}
        <div class="field">
          <label for="{id}-workspace">Workspace</label>
          <WorkspaceSelect
            id="{id}-workspace"
            bind:value={workspace}
            known={workspaces}
            invalid={workspaceProblem !== null}
            describedby={workspaceProblem ? `${id}-workspace-error` : undefined}
          />
          {#if workspaceProblem}
            <p class="error" id="{id}-workspace-error" role="alert">
              {workspaceProblem}
            </p>
          {/if}
        </div>
      {/if}
    </fieldset>
    <div class="field">
      <label for="{id}-duration">Expires</label>
      <select id="{id}-duration" bind:value={duration}>
        {#if editing}
          <option value="keep"
            >Unchanged ({editing.expires_at === null
              ? "never"
              : absoluteTime(editing.expires_at)})</option
          >
        {/if}
        {#each EXPIRY_OPTIONS as option (option.secs)}
          <option value={String(option.secs ?? 0)}>{option.label}</option>
        {/each}
      </select>
    </div>
    {#if showPrecedence}
      <div class="field">
        <label for="{id}-precedence">Precedence</label>
        <select id="{id}-precedence" bind:value={precedence}>
          <option value="">Default (5, or the rule set's)</option>
          {#each PRECEDENCES as value (value)}
            <option value={String(value)}>{value}</option>
          {/each}
        </select>
        <p class="hint">
          When rules overlap, the highest precedence decides, even over a more
          specific rule. At the same precedence the more specific rule wins.
        </p>
      </div>
    {/if}
    {#if formMessage}
      <p class="error" role="alert">{formMessage}</p>
    {/if}
    <div class="actions">
      <button
        type="button"
        class="btn"
        onclick={() => {
          open = false;
          reset();
        }}>Cancel</button
      >
      <button type="submit" class="btn primary" disabled={busy}
        >{editing ? "Save rule" : "Add rule"}</button
      >
    </div>
  </form>
</FormDialog>
