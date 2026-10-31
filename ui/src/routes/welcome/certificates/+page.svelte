<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { firstRun } from "#lib/stores/first-run.svelte.ts";
  import { networkHealth } from "#lib/stores/network-health.svelte.ts";
  import StatusMark from "#lib/welcome/StatusMark.svelte";
  import StepHeading from "#lib/welcome/StepHeading.svelte";
  import {
    canTrust,
    devCertificateLine,
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

<StepHeading
  title="Certificates"
  lead="How Puddle makes HTTPS work inside your workspaces. Nothing to decide here."
/>

<ul class="lines" aria-label="Certificates">
  {#each lines as line (line.title)}
    <li>
      <StatusMark status={line.tone} />
      <div>
        <p class="title">{line.title}</p>
        <p class="text">{line.text}</p>
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
<p class="more">
  Puddle shows this once. The details are in
  <a href="/settings/network-health">Settings &rsaquo; Network health</a>.
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
