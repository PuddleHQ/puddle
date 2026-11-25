<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import ProgressLine from "./ProgressLine.svelte";
  import StatusChip from "./StatusChip.svelte";
  import TrustedBadge from "./TrustedBadge.svelte";
  import VolumeMissingNotice from "./VolumeMissingNotice.svelte";
  import { DropdownMenu } from "bits-ui";
  import Ellipsis from "@lucide/svelte/icons/ellipsis";
  import Play from "@lucide/svelte/icons/play";
  import Plug from "@lucide/svelte/icons/plug";
  import type { ToolsState } from "#lib/workspaces/events.ts";
  import { goto } from "$app/navigation";
  import { relativeTime } from "#lib/format/relative-time.ts";
  import type { OomEvent, Progress } from "#lib/stores/workspaces.svelte.ts";
  import {
    canAttach,
    canStart,
    canStop,
    diskPercent,
    formatMib,
    isDown,
    type Workspace,
  } from "#lib/workspaces/model.ts";
  import { repoLabel } from "#lib/workspaces/repo-label.ts";
  import "#lib/theme/controls.css";

  let {
    workspace,
    now,
    waiting = 0,
    progress,
    oom,
    tools,
    onStart,
    onStop,
    onConnect,
    onDismiss,
  }: {
    workspace: Workspace;
    now: number;
    /** Requests from this workspace waiting for a decision. */
    waiting?: number;
    progress?: Progress | undefined;
    oom?: OomEvent | undefined;
    /** What the background tool install last said; nothing before it ran. */
    tools?: ToolsState | undefined;
    onStart: (w: Workspace) => void;
    onStop: (w: Workspace) => void;
    onConnect: (w: Workspace) => void;
    onDismiss: (w: Workspace) => void;
  } = $props();

  const uid = $props.id();
  const titleId = $derived(`${uid}-title`);
  const href = $derived(`/workspaces/${encodeURIComponent(workspace.id)}`);
  const down = $derived(isDown(workspace.status));
  const first = $derived(workspace.repositories[0]);
  /** What is not right with the disks, in words; the sizes themselves are on the details page. */
  const full = $derived(workspace.disk_warnings.length > 0);
  const used = $derived(
    workspace.disk_used_mib === null
      ? formatMib(workspace.disk_size_mib)
      : `${formatMib(workspace.disk_used_mib)} / ${formatMib(workspace.disk_size_mib)}`,
  );
</script>

<article class="row" aria-labelledby={titleId}>
  <div class="line">
    <span class="state"
      ><StatusChip status={workspace.status} busy={workspace.busy} /></span
    >
    <h2 id={titleId} class="name">
      <a {href}>{workspace.name}</a>
      {#if workspace.direct_ssh}<TrustedBadge />{/if}
    </h2>
    <p class="repo" title={first?.url}>
      {#if first}
        {repoLabel(first.url)}
        {#if workspace.repositories.length > 1}
          <span class="more">and {workspace.repositories.length - 1} more</span>
        {/if}
      {:else}
        <span class="more">No repository</span>
      {/if}
    </p>
    <p class="disk" class:full>{used}</p>
    <p class="waiting">
      {#if waiting > 0}
        <a
          class="count"
          href="{href}/network"
          aria-label="{waiting} waiting"
          title="{waiting} waiting">{waiting}</a
        >
      {/if}
    </p>
    <div class="acts">
      {#if down}
        <button
          type="button"
          class="btn primary"
          disabled={!canStart(workspace)}
          onclick={() => onStart(workspace)}
        >
          <Play aria-hidden="true" size={16} />Start
          <span class="visually-hidden">{workspace.name}</span>
        </button>
      {:else}
        <button
          type="button"
          class="btn primary"
          disabled={!canAttach(workspace)}
          onclick={() => onConnect(workspace)}
        >
          <Plug aria-hidden="true" size={16} />Connect
          <span class="visually-hidden"> to {workspace.name}</span>
        </button>
      {/if}
      <DropdownMenu.Root>
        <DropdownMenu.Trigger class="btn more-trigger">
          <Ellipsis aria-hidden="true" size={16} /><span class="visually-hidden"
            >More about {workspace.name}</span
          >
        </DropdownMenu.Trigger>
        <DropdownMenu.Portal>
          <DropdownMenu.Content class="row-menu" align="end" sideOffset={4}>
            <DropdownMenu.Item
              class="row-menu-item"
              onSelect={() => goto(href)}
            >
              Open details
            </DropdownMenu.Item>
            {#if !down}
              <DropdownMenu.Item
                class="row-menu-item"
                disabled={!canStop(workspace)}
                onSelect={() => onStop(workspace)}
              >
                Stop <span class="visually-hidden">{workspace.name}</span>
              </DropdownMenu.Item>
            {/if}
          </DropdownMenu.Content>
        </DropdownMenu.Portal>
      </DropdownMenu.Root>
    </div>
  </div>
  {#if workspace.disk_warnings.length > 0 || oom || (workspace.status === "volume_missing" && workspace.busy === null) || workspace.busy || progress || tools}
    <div class="notes">
      {#if workspace.disk_warnings.length > 0 || oom}
        <p class="flags">
          {#each workspace.disk_warnings as kind (kind)}
            <span class="flag warn"
              >{kind === "root" ? "Root disk" : "Volume"}
              {diskPercent(workspace, kind)} % full</span
            >
          {/each}
          {#if oom}
            <span class="flag warn"
              >Out of memory {relativeTime(oom.at, now)}</span
            >
          {/if}
        </p>
      {/if}
      {#if workspace.status === "volume_missing" && workspace.busy === null}
        <VolumeMissingNotice id={workspace.id} />
      {/if}
      <ProgressLine
        busy={workspace.busy}
        {progress}
        repositories={workspace.repositories}
        onDismiss={() => onDismiss(workspace)}
      />
      {#if tools}
        <p class="tools" class:failed={tools.phase === "failed"} role="status">
          <span class="label">Tools:</span>
          {#if tools.phase === "installing"}
            installing {tools.done} of {tools.total}
            <span class="what">({tools.tools.join(", ")})</span>
          {:else if tools.phase === "ready"}
            ready
          {:else}
            failed to install <span class="what">{tools.tools.join(", ")}</span>
            {#if tools.waiting_on.length > 0}
              <a class="flag" href="{href}/network"
                >Waiting for you to allow {tools.waiting_on.join(", ")}</a
              >
            {/if}
          {/if}
        </p>
      {/if}
    </div>
  {/if}
</article>

<style>
  .row {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--color-border-subtle);
    position: relative;
  }
  .row:hover {
    background: var(--color-surface);
  }
  /* The columns match the header drawn by the list page. */
  .line {
    display: grid;
    grid-template-columns: var(--workspace-columns);
    align-items: center;
    gap: var(--space-3);
  }
  p,
  h2 {
    margin: 0;
    min-width: 0;
  }
  .name {
    font-size: var(--text-md);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  /* The name's link covers the whole row, so a click anywhere on it opens the workspace. */
  .name a {
    display: inline-flex;
    align-items: center;
    min-height: 1.5rem;
    color: var(--color-text);
  }
  .name a::after {
    content: "";
    position: absolute;
    inset: 0;
  }
  .repo,
  .disk {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .repo {
    font-family: var(--font-mono);
  }
  .more {
    font-family: var(--font-sans);
  }
  .full {
    color: var(--color-warning);
    font-weight: 600;
  }
  .waiting {
    text-align: end;
  }
  /* A count that links to that workspace's requests: 24 px at least (WCAG 2.5.8), above the row link. */
  .count {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.5rem;
    min-height: 1.5rem;
    box-sizing: border-box;
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    color: var(--color-text);
  }
  .acts {
    position: relative;
    display: flex;
    gap: var(--space-1);
    justify-content: flex-end;
  }
  /* Above the name link's cover, so the buttons and links in a note stay clickable. */
  .notes {
    position: relative;
    display: grid;
    gap: var(--space-2);
  }
  .flags {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-3);
    font-size: var(--text-sm);
  }
  .flag {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    color: var(--color-text);
  }
  a.flag {
    position: relative;
    display: inline-flex;
    align-items: center;
    min-height: 1.5rem;
    box-sizing: border-box;
  }
  .flag.warn {
    border-color: var(--color-warning);
  }
  .tools {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
    overflow-wrap: anywhere;
  }
  .tools.failed .label {
    color: var(--color-warning);
    font-weight: 600;
  }
  .what {
    font-family: var(--font-mono);
  }
  @media (max-width: 60rem) {
    .line {
      grid-template-columns: auto 1fr auto;
      grid-template-areas:
        "state name acts"
        "repo repo waiting";
    }
    .state {
      grid-area: state;
    }
    .name {
      grid-area: name;
    }
    .acts {
      grid-area: acts;
    }
    .repo {
      grid-area: repo;
    }
    .waiting {
      grid-area: waiting;
    }
    .disk {
      display: none;
    }
  }
</style>
