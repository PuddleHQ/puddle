<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { firstRun } from "#lib/stores/first-run.svelte.ts";
  import { networkHealth } from "#lib/stores/network-health.svelte.ts";
  import StatusMark from "#lib/welcome/StatusMark.svelte";
  import StepHeading from "#lib/welcome/StepHeading.svelte";
  import {
    canTrust,
    devCertificateLine,
    MORE_DETAILS,
    PURPOSE,
    rootsLine,
    trustNote,
  } from "#lib/welcome/certificates.ts";
  import { around } from "#lib/welcome/steps.ts";
  import "#lib/theme/controls.css";

  onMount(() => void firstRun.load());

  const status = $derived(firstRun.state?.dev_certificate);
  const note = $derived(
    trustNote({
      trusting: firstRun.trusting,
      error: firstRun.trustError,
      result: firstRun.trustResult,
      status,
    }),
  );
  let noteEl: HTMLElement | undefined = $state();

  async function trust() {
    await firstRun.trust();
    // The button goes away on a yes: keep the keyboard where the outcome is read.
    if (!canTrust(firstRun.state?.dev_certificate)) noteEl?.focus();
  }

  const lines = $derived([
    devCertificateLine(firstRun.state?.dev_certificate),
    rootsLine(
      networkHealth.report,
      networkHealth.status === "failed" ||
        networkHealth.status === "unavailable",
    ),
  ]);
</script>

<StepHeading title="Certificates" lead={PURPOSE} />

<ul class="lines" aria-label="Certificates">
  {#each lines as line (line.title)}
    <li>
      <StatusMark status={line.tone} />
      <div>
        <p class="title">{line.title}</p>
        <p class="text">{line.text}</p>
        {#if line.impact || line.remedy}
          <div class="toggles">
            {#if line.impact}
              <details>
                <summary
                  >{line.tone === "ok" ? "Why it matters" : "Impact"}</summary
                >
                <p>{line.impact}</p>
              </details>
            {/if}
            {#if line.remedy}
              <details>
                <summary>Remediation</summary>
                <p>{line.remedy}</p>
              </details>
            {/if}
          </div>
        {/if}
      </div>
    </li>
  {/each}
</ul>
<div class="trust">
  {#if canTrust(status) || firstRun.trusting}
    <button
      type="button"
      class="btn"
      disabled={firstRun.trusting}
      aria-busy={firstRun.trusting}
      onclick={() => void trust()}
    >
      {firstRun.trusting ? "Waiting for Windows…" : "Trust it now"}
    </button>
  {/if}
  <p class="note" role="status" tabindex="-1" bind:this={noteEl}>{note}</p>
</div>
<details class="more-details">
  <summary>More details</summary>
  {#each MORE_DETAILS as paragraph (paragraph)}
    <p>{paragraph}</p>
  {/each}
</details>
<p class="more">
  Puddle shows this once. After setup, the details are in Settings &rsaquo;
  Network health.
</p>

<div class="actions">
  <a class="btn" href={around("certificates").back}>Back</a>
  <a class="btn primary" href={around("certificates").next}>Continue</a>
</div>

<style>
  .lines {
    margin: 0;
    padding: 0 var(--space-4);
    list-style: none;
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  li {
    display: flex;
    gap: var(--space-3);
    padding: var(--space-3) 0;
    border-bottom: 1px solid var(--color-border-subtle);
  }
  li:last-child {
    border-bottom: 0;
  }
  p {
    margin: 0;
  }
  .title {
    font-weight: 600;
  }
  .trust {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-3);
  }
  .toggles {
    display: grid;
    gap: var(--space-1);
    margin-top: var(--space-1);
  }
  summary {
    width: fit-content;
    cursor: pointer;
    font-size: var(--text-sm);
    font-weight: 600;
  }
  details > p {
    margin: var(--space-1) 0 0;
    padding: var(--space-2) var(--space-3);
    background: var(--color-surface-raised);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
  }
  .text,
  .note,
  .more {
    color: var(--color-text-muted);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
