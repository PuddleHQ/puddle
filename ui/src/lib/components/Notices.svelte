<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import {
    notices as defaultCenter,
    type Notice,
    type NoticeAction,
    type NoticeCenter,
  } from "#lib/notify/notices.svelte.ts";
  import "#lib/theme/controls.css";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  let { center = defaultCenter }: { center?: NoticeCenter } = $props();

  /** More than this are folded behind "Show all". */
  const FOLDED = 3;
  let all = $state(false);

  // Newest first.
  const list = $derived([...center.items].reverse());
  const shown = $derived(all ? list : list.slice(0, FOLDED));
  let region = $state<HTMLElement>();

  /** The action whose confirmation is open, and what its last try said. */
  let asking = $state<{ notice: Notice; action: NoticeAction } | null>(null);
  let askOpen = $state(false);
  let working = $state(false);
  let failure = $state<string | null>(null);

  function click(notice: Notice, action: NoticeAction) {
    if (!action.confirm) {
      void action.run();
      return;
    }
    failure = null;
    asking = { notice, action };
    askOpen = true;
  }

  async function confirmed(action: NoticeAction) {
    working = true;
    try {
      await action.run();
      askOpen = false;
    } catch (error) {
      failure = error instanceof Error ? error.message : String(error);
    } finally {
      working = false;
    }
  }

  /** After a dismissal the focus would fall to the page; keep it in the list, or on the page. */
  function dismiss(id: number, button: HTMLElement) {
    const buttons = [
      ...(region?.querySelectorAll<HTMLElement>("button.dismiss") ?? []),
    ];
    const at = buttons.indexOf(button);
    const next = buttons[at + 1] ?? buttons[at - 1];
    center.dismiss(id);
    if (next) next.focus();
    else document.getElementById("main")?.focus();
  }
</script>

<!-- The live region is always there, so a notice that arrives is announced; it never takes focus. -->
<div
  class="notices"
  class:empty={list.length === 0}
  role="region"
  aria-label="Notices"
  bind:this={region}
>
  <div class="live" aria-live="polite">
    {#each shown as notice (notice.id)}
      <div class="notice" class:warning={notice.tone === "warning"}>
        <div class="text">
          <p class="title">{notice.title}</p>
          {#if notice.detail}<p class="detail">{notice.detail}</p>{/if}
          {#if notice.link || notice.action}
            <div class="ways">
              {#if notice.action}
                {@const action = notice.action}
                <button
                  type="button"
                  class="btn"
                  onclick={() => click(notice, action)}>{action.label}</button
                >
              {/if}
              {#if notice.link}
                <a href={notice.link.href}>{notice.link.label}</a>
              {/if}
            </div>
          {/if}
        </div>
        <button
          type="button"
          class="btn dismiss"
          aria-label="Dismiss notice: {notice.title}"
          onclick={(e) => dismiss(notice.id, e.currentTarget)}>Dismiss</button
        >
      </div>
    {/each}
  </div>
  {#if list.length > FOLDED}
    <div class="more">
      <button
        type="button"
        class="btn"
        aria-expanded={all}
        onclick={() => (all = !all)}
        >{all ? "Show fewer" : `Show all ${list.length} notices`}</button
      >
      <button type="button" class="btn" onclick={() => center.clear()}
        >Dismiss all</button
      >
    </div>
  {/if}
</div>

{#if asking?.action.confirm}
  {@const action = asking.action}
  {@const confirm = asking.action.confirm}
  <ConfirmDialog
    bind:open={askOpen}
    title={confirm.title}
    summary={confirm.summary}
    {...confirm.detail ? { detail: confirm.detail } : {}}
    confirmLabel={confirm.confirmLabel}
    tone="deny"
    holdOpen
    closeFocus={() =>
      // The notice went with its cause, and the button the dialog opened from with it.
      asking && !center.items.some((n) => n.id === asking?.notice.id)
        ? document.getElementById("main")
        : undefined}
    {working}
    error={failure}
    onConfirm={() => void confirmed(action)}
  />
{/if}

<style>
  .notices {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-8) 0;
  }
  .notices.empty {
    padding: 0;
  }
  .live {
    display: grid;
    gap: var(--space-2);
  }
  .notice {
    display: flex;
    align-items: start;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
  }
  .notice.warning {
    border-color: var(--color-warning);
  }
  .text {
    display: grid;
    gap: var(--space-1);
    min-width: 0;
  }
  p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .title {
    font-weight: 600;
  }
  .detail {
    color: var(--color-text-muted);
  }
  .ways {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
  }
  .more {
    display: flex;
    gap: var(--space-2);
  }
</style>
