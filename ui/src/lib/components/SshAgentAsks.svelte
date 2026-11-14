<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { relativeTime } from "#lib/format/relative-time.ts";
  import type {
    SshAgentAnswer,
    SshAgentAsk,
  } from "#lib/stores/ssh-agent-asks.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import "#lib/theme/controls.css";

  let {
    asks,
    now,
    onAnswer,
  }: {
    asks: readonly SshAgentAsk[];
    /** Epoch ms the relative times are measured against; the page ticks it every 30 s. */
    now: number;
    onAnswer: (ask: SshAgentAsk, answer: SshAgentAnswer) => void;
  } = $props();

  const label = (ask: SshAgentAsk) => workspaces.labelOf(ask.workspace);
</script>

<section class="asks" aria-labelledby="ssh-asks-h">
  <div class="head">
    <h2 id="ssh-asks-h">SSH agent</h2>
    <span class="chip">{asks.length} waiting</span>
  </div>
  <p class="sub">
    A workspace is logging in over SSH with a key from your SSH agent, and its
    <span class="mono">ssh</span> waits for you. Puddle signs only that login, never
    anything else.
  </p>
  <ul>
    {#each asks as ask (ask.id)}
      <li class="ask" data-ask-id={ask.id}>
        <p class="what">
          Workspace <b class="mono">{label(ask)}</b> wants to log in to
          <b class="mono host">{ask.host}</b> with
          <span class="mono">{ask.key_type}</span>
          key <span class="mono fp">{ask.fingerprint}</span>
        </p>
        <p class="facts">
          {ask.attempts}
          {ask.attempts === 1 ? "attempt" : "attempts"} · First seen
          <time datetime={new Date(ask.first_seen).toISOString()}
            >{relativeTime(ask.first_seen, now)}</time
          >
        </p>
        <div class="acts">
          <button
            type="button"
            class="btn allow"
            aria-label="Allow once: {label(ask)} to log in to {ask.host}"
            onclick={() => onAnswer(ask, "once")}>Allow once</button
          >
          <button
            type="button"
            class="btn allow"
            aria-label="Allow for this run: {label(
              ask,
            )} to log in to {ask.host} until the workspace stops"
            onclick={() => onAnswer(ask, "run")}>Allow for this run</button
          >
          <button
            type="button"
            class="btn allow"
            aria-label="Always allow: {label(
              ask,
            )} to log in to {ask.host} with this key"
            onclick={() => onAnswer(ask, "always")}>Always allow</button
          >
          <button
            type="button"
            class="btn deny"
            aria-label="Deny: {label(ask)} to log in to {ask.host}"
            onclick={() => onAnswer(ask, "deny")}>Deny</button
          >
        </div>
      </li>
    {/each}
  </ul>
</section>

<style>
  .asks {
    margin-bottom: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
    max-width: 64rem;
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--color-border-subtle);
  }
  h2 {
    font-size: var(--text-md);
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
    white-space: nowrap;
  }
  .sub {
    margin: 0;
    padding: var(--space-2) var(--space-4) 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .ask {
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--color-border-subtle);
  }
  .ask:last-child {
    border-bottom: 0;
  }
  p {
    margin: 0;
  }
  .what,
  .fp {
    overflow-wrap: anywhere;
  }
  .facts {
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin-top: var(--space-2);
  }
</style>
