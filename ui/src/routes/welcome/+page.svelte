<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import ConfirmDialog from "#lib/components/ConfirmDialog.svelte";
  import { takeRecommendedMode } from "#lib/stores/interception.svelte.ts";
  import { finishFlow } from "#lib/welcome/finish.ts";
  import StepHeading from "#lib/welcome/StepHeading.svelte";
  import { around, WELCOME_ROOT } from "#lib/welcome/steps.ts";
  import "#lib/theme/controls.css";

  let leaving = $state(false);
  let asking = $state(false);

  /** Ends the flow here: it is not shown again. The check and the settings stay reachable. */
  async function skip() {
    leaving = true;
    await takeRecommendedMode();
    await finishFlow();
  }
</script>

<StepHeading title="Welcome to Puddle" />
<p class="body">
  Your code runs in a small virtual machine per workspace. Nothing leaves it
  unless you allow the host, and every connection is logged.
</p>
<p class="body">
  The next steps take about a minute: a check of this computer, certificates,
  how you want to connect, which hosts Puddle decrypts, and your first
  workspace.
</p>
<div class="actions">
  <button
    type="button"
    class="btn"
    disabled={leaving}
    onclick={() => (asking = true)}
  >
    Skip setup
  </button>
  <a class="btn primary" href={around("welcome").next}>Get started</a>
</div>
<ConfirmDialog
  bind:open={asking}
  title="Skip setup?"
  summary="Skip the rest of the setup?"
  detail="Puddle takes its recommended setting for reading HTTPS traffic. Everything else stays as it is, and you can change any of it later in Settings, where the system check can also be run again."
  confirmLabel="Skip setup"
  onConfirm={() => void skip()}
/>
<p class="import">
  Moving from another computer?
  <a class="btn" href={`${WELCOME_ROOT}/import`}>Import a backup</a>
</p>

<style>
  .body {
    margin: 0;
  }
  .import {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--color-text-muted);
    text-align: right;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
