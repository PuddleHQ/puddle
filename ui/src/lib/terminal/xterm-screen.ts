// SPDX-License-Identifier: AGPL-3.0-or-later
// The emulator: xterm.js behind the small `Screen` the session talks to. It lives as long as its
// sub-tab, so leaving the Terminal tab keeps the scrollback; a view mounts it into the page
// and takes it out again. No clipboard, file or image add-on is loaded, so an OSC 52 request to set
// the clipboard and an image protocol do nothing.
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal, type IDisposable } from "@xterm/xterm";
import type { PortEntry } from "#lib/workspaces/ports.ts";
import { keyAction, type KeyAction } from "./keys.ts";
import { hoverText, judgeLink } from "./links.ts";
import { readLook } from "./theme.ts";
import type { Size } from "./wire.ts";

/** What the session needs of an emulator. */
export interface Screen {
  /** Draws `bytes`, then calls `done` (the page acknowledges once they are drawn). */
  write(bytes: Uint8Array, done: () => void): void;
  /** A separator line, for a restart in the same sub-tab. */
  separator(): void;
  /** Stops reading keys: the terminal ended. */
  disable(): void;
  enable(): void;
  size(): Size;
  /** What the user typed, as the bytes to send; `onBinary` is mouse reports beyond 127. */
  onInput(listener: (text: string) => void): void;
  onBinary(listener: (bytes: Uint8Array) => void): void;
  onResize(listener: (size: Size) => void): void;
  onTitle(listener: (title: string) => void): void;
  onBell(listener: () => void): void;
  dispose(): void;
  /** The page shows what a hovered link goes to; `null` when the pointer leaves it. */
  onHover: (text: string | null) => void;
  /** F6: focus goes out of the terminal. */
  onLeave: () => void;
  /** Puts the emulator in `host` (once; later calls move it) and fits it. */
  mount(host: HTMLElement): void;
  /** Takes the emulator out of the page, keeping its contents. */
  unmount(): void;
  fit(): void;
  /** Applies the UI's colours and font as `probe` resolves them now. */
  applyLook(probe: HTMLElement): void;
  setScreenReaderMode(on: boolean): void;
  label(text: string): void;
  focus(): void;
  /** Copies the selection as plain text; `false` with nothing selected. */
  copy(): boolean;
  selectAll(): void;
  clear(): void;
  hasSelection(): boolean;
}

/** What links in the output need from the page. */
export interface LinkContext {
  forwards(): readonly PortEntry[];
  /** Opens `url` in the default browser. */
  open(url: string): void;
}

const SEPARATOR = `\r\n\u001b[2m${"-".repeat(32)}\u001b[0m\r\n`;

export class XtermScreen implements Screen {
  readonly term: Terminal;
  onHover: (text: string | null) => void = () => {};
  onLeave: () => void = () => {};
  readonly #fit = new FitAddon();
  readonly #listeners: IDisposable[] = [];

  constructor(links: LinkContext) {
    const hover = (_event: MouseEvent, text: string) =>
      this.onHover(hoverText(judgeLink(text, links.forwards())));
    const leave = () => this.onHover(null);
    const activate = (event: MouseEvent, text: string) => {
      if (!event.ctrlKey) return;
      const verdict = judgeLink(text, links.forwards());
      if (verdict.kind === "open") links.open(verdict.url);
    };
    this.term = new Terminal({
      scrollback: 10_000,
      minimumContrastRatio: 4.5,
      fontSize: 14,
      cursorBlink: false,
      linkHandler: { activate, hover, leave, allowNonHttpProtocols: false },
    });
    this.term.loadAddon(this.#fit);
    this.term.loadAddon(new WebLinksAddon(activate, { hover, leave }));
    this.term.attachCustomKeyEventHandler((event) => this.#onKey(event));
  }

  #onKey(event: KeyboardEvent): boolean {
    if (event.type !== "keydown") return true;
    const action: KeyAction = keyAction({
      key: event.key,
      ctrlKey: event.ctrlKey,
      shiftKey: event.shiftKey,
      altKey: event.altKey,
      metaKey: event.metaKey,
      altGraph: event.getModifierState("AltGraph"),
    });
    switch (action) {
      case "copy":
        event.preventDefault();
        this.copy();
        return false;
      case "paste":
        // The webview's own paste event from this key press feeds the emulator; no clipboard permission.
        return false;
      case "leave":
        event.preventDefault();
        this.onLeave();
        return false;
      case "shell-cancel":
        event.preventDefault();
        return true;
      case "shell":
        return true;
    }
  }

  mount(host: HTMLElement): void {
    if (this.term.element === undefined) this.term.open(host);
    else if (this.term.element.parentElement !== host) {
      host.append(this.term.element);
    }
    this.fit();
  }

  unmount(): void {
    this.term.element?.remove();
  }

  fit(): void {
    this.#fit.fit();
  }

  applyLook(probe: HTMLElement): void {
    const { theme, fontFamily } = readLook(probe);
    this.term.options.theme = theme;
    this.term.options.fontFamily = fontFamily;
  }

  setScreenReaderMode(on: boolean): void {
    this.term.options.screenReaderMode = on;
  }

  label(text: string): void {
    this.term.textarea?.setAttribute("aria-label", text);
  }

  focus(): void {
    this.term.focus();
  }

  /**
   * Copies the selection as plain text through the webview's own copy event, which the emulator
   * answers with the selection. With nothing selected it does nothing.
   */
  copy(): boolean {
    if (!this.term.hasSelection()) return false;
    return document.execCommand("copy");
  }

  selectAll(): void {
    this.term.selectAll();
  }

  clear(): void {
    this.term.clear();
  }

  hasSelection(): boolean {
    return this.term.hasSelection();
  }

  write(bytes: Uint8Array, done: () => void): void {
    this.term.write(bytes, done);
  }

  separator(): void {
    this.term.write(SEPARATOR);
  }

  disable(): void {
    this.term.options.disableStdin = true;
  }

  enable(): void {
    this.term.options.disableStdin = false;
  }

  size(): Size {
    return { cols: this.term.cols, rows: this.term.rows };
  }

  onInput(listener: (text: string) => void): void {
    this.#listeners.push(this.term.onData(listener));
  }

  onBinary(listener: (bytes: Uint8Array) => void): void {
    this.#listeners.push(
      this.term.onBinary((text) =>
        listener(Uint8Array.from(text, (c) => c.charCodeAt(0))),
      ),
    );
  }

  onResize(listener: (size: Size) => void): void {
    this.#listeners.push(
      this.term.onResize(({ cols, rows }) => listener({ cols, rows })),
    );
  }

  onTitle(listener: (title: string) => void): void {
    this.#listeners.push(this.term.onTitleChange(listener));
  }

  onBell(listener: () => void): void {
    this.#listeners.push(this.term.onBell(listener));
  }

  dispose(): void {
    for (const l of this.#listeners) l.dispose();
    this.term.dispose();
  }
}
