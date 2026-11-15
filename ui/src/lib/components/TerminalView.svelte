<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import type { TerminalSession } from "#lib/terminal/session.svelte.ts";
  import type { Size } from "#lib/terminal/wire.ts";

  // The emulator of one session, in the page. The emulator belongs to the session (it keeps the
  // scrollback while another tab is in view); this mounts it, keeps it fitted, follows the theme,
  // and offers the right-click menu (Copy, Select all, Clear) and the address of a hovered link.
  let {
    session,
    workspaceName,
    screenReader = false,
    focusOnOpen = false,
    onReady,
    onLeave,
  }: {
    session: TerminalSession;
    workspaceName: string;
    screenReader?: boolean;
    /** Focus the terminal once it is in the page (a terminal the user just asked for). */
    focusOnOpen?: boolean;
    /** The first measured size of a session that has not connected yet. */
    onReady: (size: Size) => void;
    /** F6: focus goes to the sub-tab row. */
    onLeave: () => void;
  } = $props();

  let host: HTMLDivElement | undefined = $state();
  let probe: HTMLSpanElement | undefined = $state();
  let hovered = $state<string | null>(null);
  let menu = $state<{ x: number; y: number } | null>(null);
  let canCopy = $state(false);
  let menuEl: HTMLDivElement | undefined = $state();

  const screen = $derived(session.screen);
  const name = $derived(`Terminal ${session.number}, ${workspaceName}`);

  onMount(() => {
    const target = host as HTMLDivElement;
    const look = () => screen.applyLook(probe as HTMLSpanElement);
    screen.onHover = (text) => (hovered = text);
    screen.onLeave = onLeave;
    look();
    screen.mount(target);
    screen.label(name);
    if (session.state === "idle") onReady(screen.size());
    if (focusOnOpen) screen.focus();
    const resized = new ResizeObserver(() => screen.fit());
    resized.observe(target);
    // A theme, palette or density change moves attributes on the root element.
    const restyled = new MutationObserver(look);
    restyled.observe(document.documentElement, { attributes: true });
    const scheme = globalThis.matchMedia("(prefers-color-scheme: dark)");
    scheme.addEventListener("change", look);
    return () => {
      resized.disconnect();
      restyled.disconnect();
      scheme.removeEventListener("change", look);
      screen.onHover = () => {};
      screen.onLeave = () => {};
      screen.unmount();
    };
  });

  $effect(() => {
    screen.setScreenReaderMode(screenReader);
  });
  $effect(() => {
    screen.label(name);
  });

  function openMenu(event: MouseEvent) {
    event.preventDefault();
    canCopy = screen.hasSelection();
    const box = (host as HTMLDivElement).getBoundingClientRect();
    menu = { x: event.clientX - box.left, y: event.clientY - box.top };
    queueMicrotask(() =>
      menuEl
        ?.querySelector<HTMLButtonElement>("button:not(:disabled)")
        ?.focus(),
    );
  }

  function closeMenu(refocus = true) {
    menu = null;
    if (refocus) screen.focus();
  }

  function choose(action: "copy" | "all" | "clear") {
    if (action === "copy") screen.copy();
    else if (action === "all") screen.selectAll();
    else screen.clear();
    closeMenu();
  }

  function menuKey(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeMenu();
      return;
    }
    const step =
      event.key === "ArrowDown" ? 1 : event.key === "ArrowUp" ? -1 : 0;
    if (step === 0) return;
    event.preventDefault();
    const items = [
      ...(menuEl as HTMLDivElement).querySelectorAll<HTMLButtonElement>(
        "button:not(:disabled)",
      ),
    ];
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    items[(at + step + items.length) % items.length]?.focus();
  }
</script>

<div class="view">
  <div
    class="screen"
    bind:this={host}
    role="group"
    aria-label={name}
    oncontextmenu={openMenu}
  ></div>
  <span class="probe" bind:this={probe} aria-hidden="true"></span>
  {#if menu}
    <div
      class="menu"
      role="menu"
      aria-label="Terminal actions"
      tabindex="-1"
      bind:this={menuEl}
      style:left="{menu.x}px"
      style:top="{menu.y}px"
      onkeydown={menuKey}
      onfocusout={(e) => {
        if (!menuEl?.contains(e.relatedTarget as Node | null)) closeMenu(false);
      }}
    >
      <button
        type="button"
        role="menuitem"
        disabled={!canCopy}
        onclick={() => choose("copy")}>Copy</button
      >
      <button type="button" role="menuitem" onclick={() => choose("all")}
        >Select all</button
      >
      <button type="button" role="menuitem" onclick={() => choose("clear")}
        >Clear</button
      >
    </div>
  {/if}
  <p class="link" aria-live="off">{hovered ?? ""}</p>
</div>

<style>
  .view {
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .screen {
    height: min(60vh, 40rem);
    min-height: 12rem;
    padding: var(--space-2);
    overflow: hidden;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-surface);
  }
  .screen :global(.xterm) {
    height: 100%;
  }
  .probe {
    position: absolute;
    width: 0;
    height: 0;
    overflow: hidden;
  }
  .menu {
    position: absolute;
    z-index: 10;
    display: flex;
    flex-direction: column;
    min-width: 9rem;
    padding: var(--space-1);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-surface-raised);
  }
  .menu button {
    min-height: var(--control-size);
    padding: var(--space-1) var(--space-3);
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--color-text);
    font: inherit;
    text-align: start;
    cursor: pointer;
  }
  .menu button:hover:not(:disabled),
  .menu button:focus-visible {
    background: var(--color-accent-subtle);
  }
  .menu button:disabled {
    color: var(--color-text-muted);
    cursor: default;
  }
  .link {
    min-height: 1.5em;
    margin: var(--space-1) 0 0;
    color: var(--color-text-muted);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
</style>
