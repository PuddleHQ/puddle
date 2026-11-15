<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import AttachButtons from "./AttachButtons.svelte";
  import ProgressLine from "./ProgressLine.svelte";
  import StatusChip from "./StatusChip.svelte";
  import TrustedBadge from "./TrustedBadge.svelte";
  import VolumeMissingNotice from "./VolumeMissingNotice.svelte";
  import type { ToolsState } from "#lib/workspaces/events.ts";
  import { relativeTime } from "#lib/format/relative-time.ts";
  import type { OomEvent, Progress } from "#lib/stores/workspaces.svelte.ts";
  import {
    diskLabel,
    diskPercent,
    diskWarned,
    formatMib,
    rootDiskLabel,
    type Workspace,
  } from "#lib/workspaces/model.ts";
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
</script>

<article class="card" aria-labelledby={titleId}>
  <header>
    <div class="who">
      <h2 id={titleId}>
        <a href="/workspaces/{encodeURIComponent(workspace.id)}"
          >{workspace.name}</a
        >
      </h2>
      <p class="repo">
        {#if workspace.repositories[0]}
          {workspace.repositories[0].url}
          {#if workspace.repositories.length > 1}
            <span class="more"
              >and {workspace.repositories.length - 1} more</span
            >
          {/if}
        {:else}
          <span class="more">No repository</span>
        {/if}
      </p>
    </div>
    {#if workspace.direct_ssh}<TrustedBadge />{/if}
    <StatusChip status={workspace.status} busy={workspace.busy} />
  </header>
  <p class="meta">
    <span>{formatMib(workspace.memory_mib)} memory</span>
    {#if rootDiskLabel(workspace) !== null}
      <span class:full={diskWarned(workspace, "root")}
        >Root disk {rootDiskLabel(workspace)}</span
      >
    {/if}
    <span class:full={diskWarned(workspace, "volume")}
      >{diskLabel(workspace)}</span
    >
    {#each workspace.disk_warnings as kind (kind)}
      <span class="flag warn"
        >{kind === "root" ? "Root disk" : "Volume"}
        {diskPercent(workspace, kind)} % full</span
      >
    {/each}
    <span>
      Created
      <time datetime={new Date(workspace.created_at).toISOString()}
        >{relativeTime(workspace.created_at, now)}</time
      >
    </span>
    {#if waiting > 0}
      <a
        class="flag"
        href="/workspaces/{encodeURIComponent(workspace.id)}/network"
        >{waiting} waiting</a
      >
    {/if}
    {#if oom}
      <span class="flag warn">Out of memory {relativeTime(oom.at, now)}</span>
    {/if}
  </p>
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
          <a
            class="flag"
            href="/workspaces/{encodeURIComponent(workspace.id)}/network"
            >Waiting for you to allow {tools.waiting_on.join(", ")}</a
          >
        {/if}
      {/if}
    </p>
  {/if}
  <div class="acts">
    <AttachButtons {workspace} {onStart} {onStop} {onConnect} />
    <a class="btn details" href="/workspaces/{encodeURIComponent(workspace.id)}"
      >Details <span class="visually-hidden">of {workspace.name}</span></a
    >
  </div>
</article>

<style>
  .card {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  header {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
  }
  .who {
    flex: 1;
    min-width: 0;
  }
  h2 {
    font-size: var(--text-lg);
    overflow-wrap: anywhere;
  }
  h2 a {
    color: var(--color-text);
  }
  .repo {
    margin: 0;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    color: var(--color-text-muted);
    overflow-wrap: anywhere;
  }
  .more {
    font-family: var(--font-sans);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-4);
    margin: 0;
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .flag {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    color: var(--color-text);
  }
  /* The link to a workspace's waiting requests is a control: 24 px tall at least (WCAG 2.5.8). */
  a.flag {
    display: inline-flex;
    align-items: center;
    min-height: 1.5rem;
    box-sizing: border-box;
  }
  .flag.warn {
    border-color: var(--color-warning);
  }
  .full {
    color: var(--color-warning);
    font-weight: 600;
  }
  .tools {
    margin: 0;
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
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .details {
    margin-inline-start: auto;
  }
</style>
