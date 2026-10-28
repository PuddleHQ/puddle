<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // The app icon picker: the built-in ducks, and the user's own image. Choosing is a draft like
  // every other setting (`onchoose`); the image is stored as soon as it is picked, because the
  // service has to check it before it can be chosen.
  import { onMount } from "svelte";
  import {
    ICON_NAMES,
    isIconChoice,
    loadIcons,
    pngUrl,
    removeIcon,
    uploadIcon,
    type AppIconChoice,
    type AppIcons,
  } from "#lib/settings/app-icon.ts";
  import "#lib/theme/controls.css";

  let {
    choice,
    stored,
    onchoose,
  }: {
    /** The icon the screen shows as chosen: an unsaved edit over what is saved. */
    choice: AppIconChoice;
    /** The icon that is saved, which the window uses now. */
    stored: AppIconChoice;
    onchoose: (choice: AppIconChoice) => void;
  } = $props();

  let icons = $state.raw<AppIcons | null>(null);
  let failure = $state<string | null>(null);
  let busy = $state(false);
  let note = $state<string | null>(null);
  let problem = $state<string | null>(null);
  let picker: HTMLInputElement | undefined = $state();

  onMount(async () => {
    const result = await loadIcons();
    if (result.ok) icons = result.value;
    else failure = result.reason;
  });

  const custom = $derived(icons?.custom_png_base64 ?? null);
  /** The image can go once nothing saved uses it. */
  const removable = $derived(custom !== null && stored !== "custom");

  function choose(value: string) {
    if (isIconChoice(value)) onchoose(value);
  }

  async function picked(input: HTMLInputElement) {
    const file = input.files?.[0];
    input.value = "";
    if (!file || !icons) return;
    busy = true;
    note = null;
    problem = null;
    const result = await uploadIcon(file);
    busy = false;
    if (!result.ok) {
      problem = result.reason;
      return;
    }
    icons = { ...icons, custom_png_base64: result.value };
    note = "Your image is ready. Save to use it.";
    onchoose("custom");
  }

  async function remove() {
    busy = true;
    note = null;
    problem = null;
    const result = await removeIcon();
    busy = false;
    if (!result.ok) {
      problem = result.reason;
      return;
    }
    if (icons) icons = { ...icons, custom_png_base64: null };
    if (choice === "custom") onchoose(stored);
    note = "Your image is removed.";
  }
</script>

<div class="icons">
  <p class="desc">
    Shown on Puddle's window, the taskbar and the tray. The duck is the default.
  </p>
  {#if failure}
    <p class="error" role="alert">The icons could not be loaded: {failure}</p>
  {:else if !icons}
    <p class="desc" role="status">Loading the icons…</p>
  {:else}
    <fieldset class="choices">
      <legend class="visually-hidden">App icon</legend>
      {#each icons.built_in as icon (icon.choice)}
        <label class="choice" class:on={choice === icon.choice}>
          <input
            type="radio"
            name="app-icon"
            value={icon.choice}
            checked={choice === icon.choice}
            onchange={(e) => choose(e.currentTarget.value)}
          />
          <img src={pngUrl(icon.png_base64)} alt="" width="48" height="48" />
          <span
            >{ICON_NAMES[icon.choice as keyof typeof ICON_NAMES] ??
              icon.choice}</span
          >
        </label>
      {/each}
      <label class="choice" class:on={choice === "custom"} class:off={!custom}>
        <input
          type="radio"
          name="app-icon"
          value="custom"
          disabled={!custom}
          checked={choice === "custom"}
          onchange={(e) => choose(e.currentTarget.value)}
        />
        {#if custom}
          <img src={pngUrl(custom)} alt="" width="48" height="48" />
        {:else}
          <span class="blank" aria-hidden="true">?</span>
        {/if}
        <span>Your image</span>
      </label>
    </fieldset>
    <div class="actions">
      <button
        type="button"
        class="btn"
        disabled={busy}
        onclick={() => picker?.click()}
      >
        {custom ? "Replace your image…" : "Choose an image…"}
      </button>
      {#if custom}
        <button
          type="button"
          class="btn"
          disabled={busy || !removable}
          onclick={() => void remove()}>Remove your image</button
        >
      {/if}
      <input
        bind:this={picker}
        type="file"
        accept="image/png,image/jpeg,image/webp"
        hidden
        data-testid="icon-file"
        onchange={(e) => void picked(e.currentTarget)}
      />
    </div>
    <p class="desc">
      PNG, JPEG or WebP, up to 8 MB and at least 32 pixels on each side. A wide
      or tall picture is fitted on a square, not cut.
      {#if custom && !removable}
        Your image is in use; choose another icon and save before removing it.
      {/if}
    </p>
    {#if stored === "custom" && !custom}
      <p class="error" role="alert">
        Your image is missing, so the duck is shown. Choose an image again or
        pick another icon.
      </p>
    {/if}
    {#if note}<p class="ok" role="status">{note}</p>{/if}
    {#if problem}<p class="error" role="alert">{problem}</p>{/if}
  {/if}
</div>

<style>
  .icons {
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
  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    margin: 0;
    padding: 0;
    border: 0;
  }
  .choice {
    display: grid;
    justify-items: center;
    gap: var(--space-1);
    width: 7.5rem;
    padding: var(--space-2);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    font-weight: 600;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .choice.on {
    border-color: var(--color-accent);
    outline: 2px solid var(--color-accent);
  }
  .choice.off {
    cursor: default;
    color: var(--color-text-muted);
  }
  .choice:has(input:focus-visible) {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }
  .choice input {
    justify-self: start;
    width: 1.5rem;
    height: 1.5rem;
    margin: 0;
  }
  .blank {
    display: grid;
    place-items: center;
    width: 48px;
    height: 48px;
    border: 1px dashed var(--color-border);
    border-radius: var(--radius-md);
    color: var(--color-text-muted);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
</style>
