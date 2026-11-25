<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // What a restore of a staged backup file would do on this machine: its origin, the counts per
  // part and what the user must still supply. Shared by the Settings card and the first-run import.
  import {
    partLabel,
    toAdd,
    type RestorePreview,
  } from "#lib/settings/restore.ts";

  let { preview }: { preview: RestorePreview } = $props();
</script>

<div class="facts">
  <h3>What this file would do</h3>
  <p class="desc">
    Made by Puddle {preview.puddle_version ??
      "(version not recorded)"}{#if preview.created_at_ms}
      on {new Date(preview.created_at_ms).toLocaleString()}{/if}.
    {toAdd(preview.parts)} entries are new to this machine.
  </p>
  <table>
    <thead>
      <tr>
        <th scope="col">Part</th>
        <th scope="col">In the file</th>
        <th scope="col">New here</th>
        <th scope="col">Differ</th>
        <th scope="col">Same</th>
        <th scope="col">Only here</th>
      </tr>
    </thead>
    <tbody>
      {#each preview.parts as part (part.name)}
        <tr data-testid={`restore-part-${part.name}`}>
          <th scope="row">{partLabel(part.name)}</th>
          <td>{part.in_backup}</td>
          <td>{part.add}</td>
          <td>{part.differ}</td>
          <td>{part.same}</td>
          <td>{part.only_here}</td>
        </tr>
      {/each}
    </tbody>
  </table>

  {#if preview.workspaces.some((w) => w.disk === "missing")}
    <p data-testid="restore-missing-disks">
      Workspaces whose disk is not on this machine are listed as "disk missing"
      and not started:
      {preview.workspaces
        .filter((w) => w.disk === "missing")
        .map((w) => w.name)
        .join(", ")}.
    </p>
  {/if}
  {#if preview.secrets_needing_value.length > 0}
    <p data-testid="restore-secrets">
      These secrets need a value on this machine (a backup never holds one):
      {preview.secrets_needing_value.map((s) => s.name).join(", ")}.
    </p>
  {/if}
  {#if preview.identity_tokens_needing_value.length > 0}
    <p data-testid="restore-identity-tokens">
      These Git identities need their token entered again on this machine (a
      backup never holds one):
      {preview.identity_tokens_needing_value
        .map((t) => `${t.identity} (${t.host})`)
        .join(", ")}.
    </p>
  {/if}
  {#if preview.running_workspaces > 0}
    <p data-testid="restore-running">
      {preview.running_workspaces} workspace{preview.running_workspaces === 1
        ? ""
        : "s"} running will stop while Puddle restarts.
    </p>
  {/if}
</div>

<style>
  .facts {
    display: grid;
    gap: var(--space-3);
  }
  p,
  h3 {
    margin: 0;
  }
  h3 {
    font-size: var(--text-md);
  }
  .desc {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  table {
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  th,
  td {
    text-align: left;
    padding: var(--space-1) var(--space-3);
  }
</style>
