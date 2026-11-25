<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import { page } from "$app/state";
  import EditorInstallNotice from "#lib/components/EditorInstallNotice.svelte";
  import RepositoriesSection from "#lib/components/RepositoriesSection.svelte";
  import RootDiskControls from "#lib/components/RootDiskControls.svelte";
  import VolumeMissingNotice from "#lib/components/VolumeMissingNotice.svelte";
  import WaitingRequests from "#lib/components/WaitingRequests.svelte";
  import { absoluteTime, relativeTime } from "#lib/format/relative-time.ts";
  import { editorProblems } from "#lib/stores/editor-problems.svelte.ts";
  import { WorkspacePortsStore } from "#lib/stores/workspace-ports.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import { canStart, diskPercent } from "#lib/workspaces/model.ts";
  import { forwardText } from "#lib/workspaces/ports.ts";
  import { tabHref } from "#lib/workspaces/tabs.ts";
  import "#lib/theme/controls.css";

  const id = $derived(page.params["id"] ?? "");
  const workspace = $derived(workspaces.list.find((w) => w.id === id));
  const editorProblem = $derived(
    workspace ? editorProblems.of(workspace.id) : null,
  );
  const editorUpdateProblem = $derived(
    workspace ? editorProblems.updateOf(workspace.id) : null,
  );
  const editorSessionUserProblem = $derived(
    workspace ? editorProblems.sessionUserOf(workspace.id) : null,
  );
  const oom = $derived(workspace ? workspaces.oom[workspace.id] : undefined);
  const ports = new WorkspacePortsStore();
  // What listens in the workspace right now: the answer to "what is this thing serving".
  const serving = $derived(
    workspace?.status === "running"
      ? ports.ports.filter((p) => p.listening)
      : [],
  );

  let now = $state(Date.now());
  let dismissedOom = $state<number | null>(null);

  onMount(() => {
    const stopEditor = editorProblems.start();
    const clock = setInterval(() => {
      now = Date.now();
    }, 30_000);
    return () => {
      stopEditor();
      clearInterval(clock);
    };
  });

  const wid = $derived(workspace?.id);
  $effect(() => {
    if (wid) return ports.start(wid);
  });
</script>

{#if workspace}
  {#each workspace.disk_warnings as kind (kind)}
    <div class="warn" role="alert" data-testid="disk-warning-{kind}">
      <TriangleAlert aria-hidden="true" size={20} />
      {#if kind === "root"}
        <div class="grow">
          <p>
            <b>Root disk nearly full:</b>
            {diskPercent(workspace, "root")} % used. It holds what was installed or
            cached outside the volume. Resetting it removes that (packages reinstall
            on the next start); the volume and your work are untouched. Puddle never
            does this by itself. Making it bigger keeps everything and needs no reset.
          </p>
          <RootDiskControls {workspace} reclaim={false} />
        </div>
      {:else}
        <p>
          <b>Volume nearly full:</b>
          {diskPercent(workspace, "volume")} % used. The volume cannot grow: delete
          what you no longer need in the workspace, then use Reclaim space in Settings.
        </p>
      {/if}
    </div>
  {/each}
  {#if oom && dismissedOom !== oom.at}
    <div class="warn" role="alert">
      <TriangleAlert aria-hidden="true" size={20} />
      <p>
        <b>Out of memory:</b> the workspace's kernel stopped
        <span class="mono">{oom.process}</span> (process {oom.pid})
        {relativeTime(oom.at, now)} because the workspace ran out of memory. The workspace
        itself is still running.
      </p>
      <a class="btn" href={tabHref(workspace.id, "settings")}>Change memory</a>
      <button
        type="button"
        class="btn"
        onclick={() => {
          dismissedOom = oom.at;
        }}>Dismiss</button
      >
    </div>
  {/if}

  <div class="stack">
    <section class="card" aria-labelledby="in-h">
      <h2 id="in-h">Get in</h2>
      <p class="line">
        Created
        <time
          datetime={new Date(workspace.created_at).toISOString()}
          title={absoluteTime(workspace.created_at)}
          >{relativeTime(workspace.created_at, now)}</time
        >.
        {#if workspace.status === "running"}
          Open a terminal here, or use Connect for the editor and SSH.
        {:else if canStart(workspace)}
          Start it to open a terminal or the editor.
        {/if}
      </p>
      {#if workspace.status === "running"}
        <p>
          <a class="btn primary" href={tabHref(workspace.id, "terminal")}
            >Open a terminal</a
          >
        </p>
      {/if}
      {#if workspace.status === "volume_missing" && workspace.busy === null}
        <VolumeMissingNotice id={workspace.id} detail />
      {/if}
      {#if editorProblem}
        <EditorInstallNotice
          title={editorProblem.title}
          detail={editorProblem.detail}
        />
      {/if}
      {#if editorUpdateProblem}
        <EditorInstallNotice
          title={editorUpdateProblem.title}
          detail={editorUpdateProblem.detail}
        />
      {/if}
      {#if editorSessionUserProblem}
        <EditorInstallNotice
          title={editorSessionUserProblem.title}
          detail={editorSessionUserProblem.detail}
        />
      {/if}
    </section>

    <WaitingRequests workspace={workspace.id} name={workspace.name} />

    <RepositoriesSection {workspace} />

    {#if serving.length > 0}
      <section class="card" aria-labelledby="serving-h">
        <h2 id="serving-h">Serving now</h2>
        <ul class="ports">
          {#each serving as entry (entry.guest_port)}
            <li>
              <span class="mono">:{entry.guest_port}</span>
              <span class="muted">{forwardText(entry)}</span>
            </li>
          {/each}
        </ul>
        <p>
          <a class="link-block" href={tabHref(workspace.id, "ports")}
            >Forward a port</a
          >
        </p>
      </section>
    {/if}
  </div>
{/if}

<style>
  .stack {
    max-width: var(--page-reading-width);
    display: grid;
    gap: var(--space-4);
  }
  .card {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  h2 {
    font-size: var(--text-md);
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--color-text-muted);
  }
  .ports {
    display: grid;
    gap: var(--space-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .ports li {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
  }
  .warn {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-3);
    margin-bottom: var(--space-4);
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--color-warning);
    border-radius: var(--radius-md);
    background: var(--color-surface-raised);
  }
  .warn p {
    flex: 1;
    min-width: 12rem;
    overflow-wrap: anywhere;
  }
  .grow {
    display: grid;
    flex: 1;
    gap: var(--space-2);
    min-width: 12rem;
  }
</style>
