<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { page } from "$app/state";
  import CredentialStoreBanner from "#lib/components/CredentialStoreBanner.svelte";
  import DuckMark from "#lib/components/DuckMark.svelte";
  import Check from "@lucide/svelte/icons/check";
  import { APP_NAME } from "#lib/nav.ts";
  import { firstRun } from "#lib/stores/first-run.svelte.ts";
  import { credentialStore } from "#lib/stores/credential-store.svelte.ts";
  import { STEPS, stepFor } from "#lib/welcome/steps.ts";

  let { children } = $props();

  // The banner follows the host's problem list for as long as the flow is open.
  $effect(() => credentialStore.start());

  const at = $derived(
    STEPS.findIndex((step) => step.id === stepFor(page.url.pathname)?.id),
  );
  // The system check is also reached from Settings, outside the flow: no step bar there.
  const inFlow = $derived(page.url.searchParams.get("from") !== "settings");

  // Puddle keeps the step the person is on, so leaving and reopening the app resumes there.
  $effect(() => {
    const step = stepFor(page.url.pathname);
    if (inFlow && step) void firstRun.recordStep(step.id);
  });
</script>

<!-- The duck and the step list stand where the app's sidebar will be, so the first run already
     looks like the product it leads into. -->
<div class="welcome" class:with-steps={inFlow}>
  <div class="side">
    <p class="brand">
      <DuckMark size={32} />
      <span>{APP_NAME}</span>
    </p>
    {#if inFlow}
      <ol class="steps" aria-label="Setup steps">
        {#each STEPS as step, index (step.id)}
          <li
            class:done={index < at}
            class:current={index === at}
            aria-current={index === at ? "step" : undefined}
          >
            {#if index < at}
              <Check class="tick" size={16} aria-hidden="true" />
              <span class="visually-hidden">Done: </span>
            {:else}
              <span class="number" aria-hidden="true">{index + 1}</span>
            {/if}
            {step.label}
          </li>
        {/each}
      </ol>
    {/if}
  </div>
  <div class="step">
    <CredentialStoreBanner store={credentialStore} />
    {@render children()}
  </div>
</div>

<style>
  .welcome {
    display: grid;
    gap: var(--space-4);
    align-items: start;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }
  .side {
    display: grid;
    gap: var(--space-4);
    align-content: start;
  }
  .steps {
    display: grid;
    gap: var(--space-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .steps li {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-3);
    border-radius: var(--radius-md);
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .steps li.done {
    color: var(--color-text);
  }
  .steps li.current {
    background: var(--color-accent-subtle);
    color: var(--color-text);
    font-weight: 600;
  }
  .number {
    display: inline-block;
    min-width: 1rem;
    text-align: center;
  }
  .step {
    display: grid;
    gap: var(--space-4);
    max-width: var(--page-reading-width);
  }
  .welcome :global(.btn:disabled) {
    opacity: 0.6;
    cursor: default;
  }
  /* A wide window: the step list is a column on the left, as the sidebar will be. */
  @media (min-width: 900px) {
    .welcome.with-steps {
      grid-template-columns: var(--sidebar-width) minmax(0, 1fr);
      gap: var(--space-8);
    }
  }
  @media (max-width: 760px) {
    .number {
      display: none;
    }
  }
</style>
