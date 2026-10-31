<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { workspaceNameError } from "#lib/rules/model.ts";
  import {
    hostProblem,
    methodsProblem,
    pathProblem,
    splitMethods,
    type NewRequestRule,
    type RequestField,
  } from "#lib/rules/request-rules.ts";
  import type { RequestRuleRefusal } from "#lib/stores/request-rules.svelte.ts";
  import FormDialog from "./FormDialog.svelte";

  // The form keeps what was typed while it is closed, so a refused rule (or a cancelled confirm) can
  // be corrected instead of retyped. `onSubmit` gets a complete request; it returns `null` when it
  // has taken over (the confirm step) and the refusal otherwise.
  let {
    open = $bindable(false),
    workspaces,
    error = $bindable(null),
    onSubmit,
  }: {
    open?: boolean;
    /** Workspaces to suggest in the workspace field. */
    workspaces: string[];
    error?: RequestRuleRefusal | null;
    onSubmit: (rule: NewRequestRule) => Promise<RequestRuleRefusal | null>;
  } = $props();

  const id = $props.id();
  let host = $state("");
  let methods = $state("");
  let path = $state("");
  let scope = $state<"workspace" | "global">("workspace");
  let workspace = $state("");
  let busy = $state(false);
  let problems = $state<Partial<Record<RequestField, string>>>({});

  const message = (field: RequestField) =>
    problems[field] ?? (error?.field === field ? error.message : null);
  const hostMessage = $derived(message("host"));
  const methodsMessage = $derived(message("methods"));
  const pathMessage = $derived(message("path"));
  const workspaceMessage = $derived(message("workspace"));
  const formMessage = $derived(message("form"));

  function describedBy(field: string, hint: boolean, shown: string | null) {
    return `${hint ? `${id}-${field}-hint` : ""}${shown ? ` ${id}-${field}-error` : ""}`.trim();
  }

  function reset() {
    host = "";
    methods = "";
    path = "";
    scope = "workspace";
    workspace = "";
    problems = {};
    error = null;
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    error = null;
    const found: Partial<Record<RequestField, string>> = {};
    const checks: [RequestField, string | null][] = [
      ["host", hostProblem(host)],
      ["methods", methodsProblem(methods)],
      ["path", pathProblem(path.trim())],
      [
        "workspace",
        scope === "workspace" ? workspaceNameError(workspace.trim()) : null,
      ],
    ];
    for (const [field, problem] of checks) if (problem) found[field] = problem;
    problems = found;
    if (Object.keys(found).length > 0) return;
    busy = true;
    try {
      const refusal = await onSubmit({
        host: host.trim(),
        methods: splitMethods(methods),
        path: path.trim(),
        workspace: scope === "global" ? null : (workspace.trim() as never),
      });
      if (refusal) error = refusal;
    } finally {
      busy = false;
    }
  }

  /** Empties the form; the page calls it after a rule was added. */
  export function clear(): void {
    reset();
  }
</script>

<FormDialog
  bind:open
  title="Add a method and path rule"
  description="The rule refuses the methods and paths you name on one host. Nothing else changes: it never allows anything."
>
  <form onsubmit={submit} novalidate>
    <div class="field">
      <label for="{id}-host">Host</label>
      <input
        id="{id}-host"
        type="text"
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        placeholder="api.example.com or *.example.com"
        bind:value={host}
        aria-invalid={hostMessage ? "true" : undefined}
        aria-describedby={describedBy("host", true, hostMessage)}
      />
      <p class="hint" id="{id}-host-hint">
        Puddle decrypts this host while the rule exists, so it can read the
        requests. <span class="mono">*.example.com</span> covers every name under
        example.com, not example.com itself.
      </p>
      {#if hostMessage}
        <p class="error" id="{id}-host-error" role="alert">{hostMessage}</p>
      {/if}
    </div>
    <div class="field">
      <label for="{id}-methods">Methods</label>
      <input
        id="{id}-methods"
        type="text"
        autocomplete="off"
        autocapitalize="characters"
        spellcheck="false"
        placeholder="DELETE, PUT"
        bind:value={methods}
        aria-invalid={methodsMessage ? "true" : undefined}
        aria-describedby={describedBy("methods", true, methodsMessage)}
      />
      <p class="hint" id="{id}-methods-hint">
        Separate with commas. Leave it empty to refuse every method.
      </p>
      {#if methodsMessage}
        <p class="error" id="{id}-methods-error" role="alert">
          {methodsMessage}
        </p>
      {/if}
    </div>
    <div class="field">
      <label for="{id}-path">Path</label>
      <input
        id="{id}-path"
        type="text"
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        placeholder="/repos/**"
        bind:value={path}
        aria-invalid={pathMessage ? "true" : undefined}
        aria-describedby={describedBy("path", true, pathMessage)}
      />
      <p class="hint" id="{id}-path-hint">
        <span class="mono">/gists</span> is that path alone,
        <span class="mono">*</span> is one segment, and a closing
        <span class="mono">/**</span> is everything below.
        <span class="mono">/**</span>
        is every path. The query string is never read. Capitals and encoded characters
        count as the same path, so a rule may refuse a little more than you wrote.
      </p>
      {#if pathMessage}
        <p class="error" id="{id}-path-error" role="alert">{pathMessage}</p>
      {/if}
    </div>
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
      {#if scope === "workspace"}
        <div class="field">
          <label for="{id}-workspace">Workspace name</label>
          <input
            id="{id}-workspace"
            type="text"
            list="{id}-workspaces"
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            bind:value={workspace}
            aria-invalid={workspaceMessage ? "true" : undefined}
            aria-describedby={workspaceMessage
              ? `${id}-workspace-error`
              : undefined}
          />
          <datalist id="{id}-workspaces">
            {#each workspaces as name (name)}<option value={name}
              ></option>{/each}
          </datalist>
          {#if workspaceMessage}
            <p class="error" id="{id}-workspace-error" role="alert">
              {workspaceMessage}
            </p>
          {/if}
        </div>
      {/if}
    </fieldset>
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
      <button type="submit" class="btn primary" disabled={busy}>Add rule</button
      >
    </div>
  </form>
</FormDialog>
