<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import { page } from "$app/state";
  import EditorInstallNotice from "#lib/components/EditorInstallNotice.svelte";
  import RepositoriesSection from "#lib/components/RepositoriesSection.svelte";
  import ConfirmDialog from "#lib/components/ConfirmDialog.svelte";
  import GrowRootDiskDialog from "#lib/components/GrowRootDiskDialog.svelte";
  import StatusChip from "#lib/components/StatusChip.svelte";
  import VolumeMissingNotice from "#lib/components/VolumeMissingNotice.svelte";
  import { api } from "#lib/api/client.ts";
  import { copyText } from "#lib/welcome/doctor.ts";
  import { absoluteTime, relativeTime } from "#lib/format/relative-time.ts";
  import { isExpired, workspaceOf } from "#lib/rules/model.ts";
  import { editorProblems } from "#lib/stores/editor-problems.svelte.ts";
  import { pending } from "#lib/stores/pending.svelte.ts";
  import { rulesStore } from "#lib/stores/rules.svelte.ts";
  import { workspaceActions as actions } from "#lib/stores/workspace-actions.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import {
    canDelete,
    canGrowRootDisk,
    canReclaim,
    canResetRootDisk,
    diskLabel,
    diskPercent,
    diskWarned,
    formatMib,
    rootDiskLabel,
  } from "#lib/workspaces/model.ts";
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

  let now = $state(Date.now());
  let dismissedOom = $state<number | null>(null);
  let resetOpen = $state(false);
  function openReset() {
    resetOpen = true;
  }
  let growOpen = $state(false);
  function openGrow() {
    growOpen = true;
  }
  let copied = $state<"yes" | "no" | null>(null);
  async function copyId() {
    if (!workspace) return;
    copied = (await copyText(workspace.id)) ? "yes" : "no";
  }
  let memorySource = $state<"workspace" | "global" | "default" | null>(null);

  onMount(() => {
    const stopRules = rulesStore.start();
    const stopEditor = editorProblems.start();
    const clock = setInterval(() => {
      now = Date.now();
    }, 30_000);
    return () => {
      stopRules();
      stopEditor();
      clearInterval(clock);
    };
  });

  // Where the memory figure comes from, so "workspace override" and "global default" say true.
  const wid = $derived(workspace?.id);
  const memoryMib = $derived(workspace?.memory_mib);
  $effect(() => {
    if (!wid) return;
    void memoryMib; // read again when the figure changes
    void (async () => {
      try {
        const { data } = await api.GET("/api/settings/workspaces/{workspace}", {
          params: { path: { workspace: wid } },
        });
        memorySource = data?.effective.memory.source ?? null;
      } catch {
        memorySource = null;
      }
    })();
  });

  const waiting = $derived(
    pending.rows.filter((r) => r.request.workspace === workspace?.id).length,
  );
  const ruleCounts = $derived.by(() => {
    const active = rulesStore.rules.filter((r) => !isExpired(r, now));
    return {
      workspace: active.filter((r) => workspaceOf(r) === workspace?.id).length,
      global: active.filter((r) => workspaceOf(r) === null).length,
    };
  });
  const plural = (n: number, one: string, many: string) =>
    `${n} ${n === 1 ? one : many}`;
  const networkSummary = $derived(
    [
      `${waiting} waiting`,
      plural(ruleCounts.workspace, "workspace rule", "workspace rules"),
      plural(
        ruleCounts.global,
        "rule for every workspace",
        "rules for every workspace",
      ),
    ].join(" · "),
  );
</script>

{#if workspace}
  {#each workspace.disk_warnings as kind (kind)}
    <div class="warn" role="alert" data-testid="disk-warning-{kind}">
      <TriangleAlert aria-hidden="true" size={20} />
      {#if kind === "root"}
        <p>
          <b>Root disk nearly full:</b>
          {diskPercent(workspace, "root")} % used. It holds what was installed or
          cached outside the volume. Resetting it removes that (packages reinstall
          on the next start); the volume and your work are untouched. Puddle never
          does this by itself. Making it bigger keeps everything and needs no reset.
        </p>
        <button
          type="button"
          class="btn primary"
          disabled={!canGrowRootDisk(workspace)}
          onclick={openGrow}>Make root disk bigger…</button
        >
        <button
          type="button"
          class="btn"
          disabled={!canResetRootDisk(workspace)}
          onclick={openReset}>Reset root disk…</button
        >
        {#if workspace.busy === null && !canResetRootDisk(workspace)}
          <span class="hint">Stop the workspace first.</span>
        {/if}
      {:else}
        <p>
          <b>Volume nearly full:</b>
          {diskPercent(workspace, "volume")} % used. The volume cannot grow: delete
          what you no longer need in the workspace, then use Reclaim space.
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

  <div class="grid">
    <section class="card" aria-labelledby="status-h">
      <h2 id="status-h">Status</h2>
      <dl>
        <dt>Workspace id</dt>
        <dd class="id-row">
          <span class="mono">{workspace.id}</span>
          <button
            type="button"
            class="btn"
            aria-label="Copy the workspace id"
            onclick={() => void copyId()}
            >{copied === "yes"
              ? "Copied"
              : copied === "no"
                ? "Could not copy"
                : "Copy"}</button
          >
        </dd>
        <dt>State</dt>
        <dd><StatusChip status={workspace.status} busy={workspace.busy} /></dd>
        <dt>Created</dt>
        <dd>
          <time
            datetime={new Date(workspace.created_at).toISOString()}
            title={absoluteTime(workspace.created_at)}
            >{relativeTime(workspace.created_at, now)}</time
          >
        </dd>
        <dt>Image</dt>
        <dd class="mono">{workspace.image}</dd>
      </dl>
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

    <RepositoriesSection {workspace} />

    <section class="card" aria-labelledby="resources-h">
      <h2 id="resources-h">Resources</h2>
      <dl>
        <dt>Memory</dt>
        <dd>
          {formatMib(workspace.memory_mib)}
          {#if memorySource}
            <span class="chip"
              >{memorySource === "workspace"
                ? "workspace override"
                : memorySource === "global"
                  ? "global default"
                  : "Puddle's default"}</span
            >
          {/if}
        </dd>
        {#if rootDiskLabel(workspace) !== null}
          <dt>Root disk</dt>
          <dd class:full={diskWarned(workspace, "root")}>
            {rootDiskLabel(workspace)}{diskWarned(workspace, "root")
              ? " (nearly full)"
              : ""}
          </dd>
        {/if}
        <dt>Disk</dt>
        <dd class:full={diskWarned(workspace, "volume")}>
          {diskLabel(workspace)}{diskWarned(workspace, "volume")
            ? " (nearly full)"
            : ""}
        </dd>
        <dt>Volume</dt>
        <dd class="mono">ws-{workspace.id}</dd>
      </dl>
      <p class="hint">
        Reclaiming space gives disk the workspace no longer uses back to your
        computer. Resetting the root disk (a stopped workspace) removes what was
        installed outside the volume. Making it bigger works for a running or a
        stopped workspace and is kept when it is reset.
      </p>
      <div class="row">
        <button
          type="button"
          class="btn"
          disabled={!canGrowRootDisk(workspace)}
          onclick={openGrow}>Make root disk bigger…</button
        >
        <button
          type="button"
          class="btn"
          disabled={!canReclaim(workspace)}
          onclick={() => void actions.reclaim(workspace)}>Reclaim space</button
        >
        <button
          type="button"
          class="btn"
          disabled={!canResetRootDisk(workspace)}
          onclick={openReset}>Reset root disk…</button
        >
      </div>
    </section>

    <section class="card" aria-labelledby="network-h">
      <h2 id="network-h">Network</h2>
      <p>{networkSummary}</p>
      <p>
        <a class="link-block" href={tabHref(workspace.id, "network")}
          >Open the network tab</a
        >
      </p>
    </section>

    <section class="card danger" aria-labelledby="danger-h">
      <h2 id="danger-h">Delete workspace</h2>
      {#if workspace.status === "volume_missing"}
        <p class="hint">
          The disk is already gone, so deleting only removes the workspace from
          Puddle.
        </p>
      {:else}
        <p class="hint">
          Deleting removes the workspace and its disk. Puddle first lists
          uncommitted changes, unpushed commits and stashes.
        </p>
      {/if}
      {#if !canDelete(workspace) && workspace.busy === null}
        <p class="hint">Stop the workspace before deleting it.</p>
      {/if}
      <div class="row">
        <button
          type="button"
          class="btn deny"
          disabled={!canDelete(workspace)}
          onclick={() => void actions.askDelete(workspace)}
        >
          <Trash2 aria-hidden="true" size={16} />{actions.checking ===
          workspace.id
            ? "Checking…"
            : "Delete workspace…"}
        </button>
      </div>
    </section>
  </div>
  <GrowRootDiskDialog
    bind:open={growOpen}
    {workspace}
    onSubmit={(gib) => actions.growRootDisk(workspace, gib)}
  />
  <ConfirmDialog
    bind:open={resetOpen}
    title="Reset the root disk of {workspace.name}?"
    summary="Everything installed or cached outside the volume is removed."
    detail="Packages and tools installed outside the volume, the home directory and tool caches are lost, including any file kept there that is not in a repository on the volume; packages reinstall and caches rebuild on the next start. The volume, with your repositories and files, is untouched."
    confirmLabel="Reset root disk"
    tone="deny"
    onConfirm={() => void actions.resetRootDisk(workspace)}
  />
{/if}

<style>
  .id-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(24rem, 100%), 1fr));
    gap: var(--space-4);
    align-items: start;
  }
  .card {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .card.danger {
    border-color: var(--color-danger);
  }
  h2 {
    font-size: var(--text-md);
  }
  p {
    margin: 0;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--space-2) var(--space-4);
    margin: 0;
  }
  dt {
    color: var(--color-text-muted);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .hint {
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .chip {
    margin-inline-start: var(--space-2);
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
    white-space: nowrap;
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
  .full {
    color: var(--color-warning);
    font-weight: 600;
  }
  .btn:disabled {
    cursor: default;
    opacity: 0.6;
  }
</style>
