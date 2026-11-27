<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // The export part of the Log settings: one button that downloads a bundle of the recent logs,
  // the versions and the settings, and what the file does and does not hold.
  import { sizeText } from "#lib/settings/backup.ts";
  import { exportLogBundle } from "#lib/settings/logs.ts";
  import "#lib/theme/controls.css";

  let busy = $state(false);
  let outcome = $state<{ ok: boolean; text: string } | null>(null);

  async function run() {
    busy = true;
    outcome = null;
    const result = await exportLogBundle();
    busy = false;
    outcome = result.ok
      ? {
          ok: true,
          text: `Saved ${result.name} (${sizeText(result.bytes)}) to your downloads.`,
        }
      : {
          ok: false,
          text: `The log bundle could not be made: ${result.reason}`,
        };
  }
</script>

<div class="bundle">
  <p class="desc">
    Saves Puddle's recent logs, its version, your computer's type and your
    settings in one file, to attach when you ask for help.
  </p>
  <div class="actions">
    <button
      type="button"
      class="btn"
      data-testid="export-log-bundle"
      disabled={busy}
      onclick={() => void run()}
    >
      {busy ? "Exporting…" : "Export log bundle"}
    </button>
  </div>
  {#if outcome}
    <p
      class={outcome.ok ? "ok" : "error"}
      role={outcome.ok ? "status" : "alert"}
      data-testid={outcome.ok ? "log-bundle-done" : "log-bundle-problem"}
    >
      {outcome.text}
    </p>
  {/if}
  <p class="desc">
    Tokens, passwords, cookies and the user and query of web addresses are cut
    out. It still names your hosts and workspaces, so read it before you share
    it.
  </p>
</div>

<style>
  .bundle {
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
