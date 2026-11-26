<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { finishFlow } from "#lib/welcome/finish.ts";
  import StepHeading from "#lib/welcome/StepHeading.svelte";
  import { around } from "#lib/welcome/steps.ts";
  import "#lib/theme/controls.css";

  let leaving = $state(false);

  /** The flow ends here. */
  async function finish() {
    leaving = true;
    await finishFlow();
  }
</script>

<StepHeading
  title="You're all set"
  lead="Puddle is ready. Create your first workspace from Workspaces whenever you like."
/>

<ul class="where">
  <li>
    <b>Settings</b> holds every choice you made here: the editor, how Puddle reads
    HTTPS traffic, the look, and the system check.
  </li>
  <li><b>Rules</b> decide what a workspace may reach.</li>
  <li><b>Inbox</b> shows what is waiting for your answer.</li>
</ul>

<p class="body">
  Feedback is very welcome. Tell us what is unclear or missing in the issues of
  PuddleHQ/puddle on GitHub.
</p>

<div class="actions">
  <a class="btn" href={around("done").back}>Back</a>
  <span class="grow"></span>
  <button
    type="button"
    class="btn primary"
    disabled={leaving}
    onclick={() => void finish()}
  >
    Open Puddle
  </button>
</div>

<style>
  .body,
  .where {
    margin: 0;
    color: var(--color-text-muted);
  }
  .where {
    padding-left: var(--space-4);
    display: grid;
    gap: var(--space-1);
  }
  .where b {
    color: var(--color-text);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  .grow {
    flex: 1;
  }
</style>
