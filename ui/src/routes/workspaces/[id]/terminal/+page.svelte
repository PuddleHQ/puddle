<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { tick } from "svelte";
  import Bell from "@lucide/svelte/icons/bell";
  import Plus from "@lucide/svelte/icons/plus";
  import X from "@lucide/svelte/icons/x";
  import "@xterm/xterm/css/xterm.css";
  import { page } from "$app/state";
  import TerminalView from "#lib/components/TerminalView.svelte";
  import { terminals, setForwards } from "#lib/stores/terminals.svelte.ts";
  import { WorkspacePortsStore } from "#lib/stores/workspace-ports.svelte.ts";
  import { workspaceActions as actions } from "#lib/stores/workspace-actions.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import { tabHref } from "#lib/workspaces/tabs.ts";
  import { canStart } from "#lib/workspaces/model.ts";
  import type { TerminalSession } from "#lib/terminal/session.svelte.ts";
  import "#lib/theme/controls.css";

  const id = $derived(page.params["id"] ?? "");
  const workspace = $derived(workspaces.list.find((w) => w.id === id));
  const wid = $derived(workspace?.id);
  const ports = new WorkspacePortsStore();
  const fid = $props.id();

  // A terminal opens in a running workspace, or one that is starting (it waits for the boot).
  const usable = $derived(
    workspace?.status === "running" || workspace?.status === "starting",
  );
  const sessions = $derived(wid ? terminals.of(wid) : []);
  const shown = $derived(wid ? terminals.selected(wid) : undefined);
  const limit = $derived(wid ? terminals.limit(wid) : null);
  let focusNew = $state(false);
  let strip: HTMLDivElement | undefined = $state();
  let addButton: HTMLButtonElement | undefined = $state();

  $effect(() => {
    if (wid) return ports.start(wid);
  });
  $effect(() => {
    if (wid) setForwards(wid, ports.ports);
  });

  function add() {
    if (!wid) return;
    focusNew = terminals.add(wid) !== null;
  }

  async function close(session: TerminalSession) {
    const own = terminals.of(session.workspace);
    const at = own.indexOf(session);
    terminals.remove(session);
    await tick();
    const next = own[at + 1] ?? own[at - 1];
    if (next) tabButton(next)?.focus();
    else addButton?.focus();
  }

  const tabButton = (session: TerminalSession) =>
    strip?.querySelector<HTMLButtonElement>(
      `[data-terminal="${session.number}"]`,
    );

  function pick(session: TerminalSession) {
    if (wid) terminals.select(wid, session);
  }

  function tabKey(event: KeyboardEvent, at: number) {
    const step =
      event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    const last = sessions.length - 1;
    const to =
      event.key === "Home" ? 0 : event.key === "End" ? last : at + step;
    if (
      to === at ||
      (step === 0 && event.key !== "Home" && event.key !== "End")
    ) {
      return;
    }
    event.preventDefault();
    const target = sessions[(to + sessions.length) % sessions.length];
    if (!target) return;
    pick(target);
    void tick().then(() => tabButton(target)?.focus());
  }

  function leave() {
    if (shown) tabButton(shown)?.focus();
  }

  async function restart(session: TerminalSession) {
    focusNew = true;
    await session.restart();
  }
</script>

{#if workspace}
  <section aria-labelledby="{fid}-h">
    <h2 id="{fid}-h">Terminal</h2>
    {#if sessions.length === 0 && !usable}
      <div class="empty">
        <p>Start the workspace to open a terminal.</p>
        <button
          type="button"
          class="btn primary"
          disabled={!canStart(workspace)}
          onclick={() => actions.start(workspace)}>Start</button
        >
      </div>
    {:else}
      <div class="bar">
        <div
          class="tabs"
          role="tablist"
          aria-label="Terminals"
          bind:this={strip}
        >
          {#each sessions as session, at (session.number)}
            <button
              type="button"
              role="tab"
              class="tab"
              data-terminal={session.number}
              id="{fid}-tab-{session.number}"
              aria-selected={session === shown}
              aria-controls="{fid}-panel"
              aria-label={`Terminal ${session.number}`}
              aria-describedby="{fid}-state-{session.number}"
              tabindex={session === shown ? 0 : -1}
              onclick={() => pick(session)}
              onkeydown={(e) => tabKey(e, at)}
            >
              <span aria-hidden="true">{session.label}</span>
              {#if session.bell}<Bell aria-hidden="true" size={14} />{/if}
              <span class="visually-hidden" id="{fid}-state-{session.number}"
                >{session.bell ? "Bell rang. " : ""}{session.state === "ended"
                  ? "Ended."
                  : ""}</span
              >
            </button>
          {/each}
        </div>
        <button
          type="button"
          class="btn"
          bind:this={addButton}
          disabled={!usable || limit !== null}
          aria-describedby={limit ? `${fid}-limit` : undefined}
          onclick={add}
        >
          <Plus aria-hidden="true" size={16} />New terminal
        </button>
        {#if shown}
          <button type="button" class="btn" onclick={() => close(shown)}>
            <X aria-hidden="true" size={16} />Close Terminal {shown.number}
          </button>
        {/if}
        <label class="switch">
          <input type="checkbox" bind:checked={terminals.screenReader} />
          Screen reader mode
        </label>
      </div>
      {#if limit}
        <p class="muted" id="{fid}-limit">{limit}</p>
      {/if}
      {#if !usable}
        <p class="muted">Start the workspace to open a terminal.</p>
      {/if}
      {#if terminals.problem}
        <p class="error" role="alert">{terminals.problem}</p>
      {/if}
      {#if shown}
        <div
          class="panel"
          id="{fid}-panel"
          role="tabpanel"
          aria-labelledby="{fid}-tab-{shown.number}"
        >
          {#key shown.number}
            <TerminalView
              session={shown}
              workspaceName={workspace.name}
              screenReader={terminals.screenReader}
              focusOnOpen={focusNew}
              onReady={(size) => void terminals.connect(shown, size)}
              onLeave={leave}
            />
          {/key}
          {#if shown.state === "live" && !shown.output && workspace.status === "starting"}
            <p class="muted" role="status">
              Waiting for the workspace to finish starting
            </p>
          {/if}
          {#if shown.end}
            <div class="end" role="status">
              <p>{shown.end.line}</p>
              {#if shown.end.detail}<p class="muted">{shown.end.detail}</p>{/if}
              {#if shown.end.action === "restart"}
                <button type="button" class="btn" onclick={() => restart(shown)}
                  >Restart</button
                >
              {:else if shown.end.action === "start" && !usable}
                <button
                  type="button"
                  class="btn"
                  disabled={!canStart(workspace)}
                  onclick={() => actions.start(workspace)}>Start</button
                >
              {:else if shown.end.action === "overview"}
                <a class="btn" href={tabHref(workspace.id, "")}>Overview</a>
              {:else}
                <button
                  type="button"
                  class="btn"
                  disabled={!usable || limit !== null}
                  onclick={add}>New terminal</button
                >
              {/if}
            </div>
          {/if}
        </div>
      {:else if usable}
        <p class="muted">
          No terminal is open. Choose New terminal to open one.
        </p>
      {/if}
    {/if}
  </section>
{:else}
  <p class="muted">Loading&hellip;</p>
{/if}

<style>
  section {
    display: grid;
    gap: var(--space-3);
    margin-bottom: var(--space-6);
    max-width: 80rem;
    min-width: 0;
  }
  h2 {
    font-size: var(--text-lg);
    margin: 0;
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
  }
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
    min-width: 0;
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    min-height: var(--control-size);
    max-width: 16rem;
    padding: var(--space-1) var(--space-3);
    overflow: hidden;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-surface);
    color: var(--color-text);
    font: inherit;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
  }
  .tab[aria-selected="true"] {
    background: var(--color-accent-subtle);
    border-color: var(--color-accent);
  }
  .panel {
    min-width: 0;
  }
  .switch {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-sm);
  }
  .end {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
    margin-top: var(--space-2);
  }
  .end p {
    margin: 0;
  }
  .empty {
    display: grid;
    gap: var(--space-3);
    justify-items: start;
  }
  .error {
    color: var(--color-danger);
    margin: 0;
  }
  .muted {
    color: var(--color-text-muted);
    margin: 0;
  }
</style>
