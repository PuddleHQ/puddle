<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // The Back up part of Settings: one button that downloads a backup of Puddle's own state, and
  // what the file does and does not hold.
  import { backUpNow, sizeText } from "#lib/settings/backup.ts";
  import "#lib/theme/controls.css";

  let busy = $state(false);
  let done = $state<{ name: string; bytes: number } | null>(null);
  let problem = $state<string | null>(null);

  async function run() {
    busy = true;
    done = null;
    problem = null;
    const result = await backUpNow();
    busy = false;
    if (result.ok) done = { name: result.name, bytes: result.bytes };
    else problem = result.reason;
  }
</script>

<div class="backup">
  <p class="desc">
    Saves your rules, settings, workspace list, Git identities and activity log
    in one file, as they are right now. Puddle keeps running while it works.
  </p>
  <div class="actions">
    <button
      type="button"
      class="btn"
      data-testid="back-up-now"
      disabled={busy}
      onclick={() => void run()}
    >
      {busy ? "Backing up…" : "Back up now"}
    </button>
  </div>
  {#if done}
    <p class="ok" role="status" data-testid="backup-done">
      Saved {done.name} ({sizeText(done.bytes)}) to your downloads.
    </p>
  {/if}
  {#if problem}
    <p class="error" role="alert" data-testid="backup-problem">
      The backup could not be made: {problem}
    </p>
  {/if}
  <p class="desc">
    The file holds no token, password or key, and not your workspaces' disks:
    after you use it on a new computer you enter secrets again and recreate
    workspaces whose disk is not there. It still names your hosts and
    workspaces, so keep it as private as Puddle's data folder.
  </p>
</div>

<style>
  .backup {
    display: grid;
    gap: var(--space-3);
  }
  p {
    margin: 0;
  }
  .desc {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .error {
    color: var(--color-danger);
  }
  .ok {
    color: var(--color-success);
  }
  .actions {
    display: flex;
    gap: var(--space-3);
  }
</style>
