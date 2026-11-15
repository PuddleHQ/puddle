// SPDX-License-Identifier: AGPL-3.0-or-later
// Stand-ins for the emulator and the socket of a terminal, and the two jsdom gaps the real emulator
// meets (no `matchMedia`, no canvas).
import type { SocketLike } from "#lib/terminal/session.svelte.ts";
import type { Screen } from "#lib/terminal/xterm-screen.ts";
import type { Size } from "#lib/terminal/wire.ts";

export class FakeSocket implements SocketLike {
  binaryType = "blob";
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  sent: (string | Uint8Array)[] = [];
  closed = false;
  constructor(
    readonly url: string,
    readonly protocols: string[],
  ) {}
  send(data: string | Uint8Array): void {
    this.sent.push(data);
  }
  close(): void {
    this.closed = true;
  }
  open(): void {
    this.onopen?.();
  }
  /** What the host sends: bytes as a binary message, anything else as text. */
  receive(data: string | Uint8Array): void {
    this.onmessage?.({
      data:
        typeof data === "string"
          ? data
          : data.buffer.slice(data.byteOffset, data.byteOffset + data.length),
    });
  }
  /** The text messages the page sent, parsed. */
  controls(): unknown[] {
    return this.sent
      .filter((m): m is string => typeof m === "string")
      .map((m) => JSON.parse(m) as unknown);
  }
  /** The binary messages the page sent, as text. */
  typed(): string[] {
    return this.sent
      .filter((m): m is Uint8Array => typeof m !== "string")
      .map((m) => new TextDecoder().decode(m));
  }
}

export class FakeScreen implements Screen {
  written: { bytes: Uint8Array; done: () => void }[] = [];
  separators = 0;
  enabled = true;
  disposed = false;
  cols = 80;
  rows = 24;
  onHover: (text: string | null) => void = () => {};
  onLeave: () => void = () => {};
  input: (text: string) => void = () => {};
  binary: (bytes: Uint8Array) => void = () => {};
  resize: (size: Size) => void = () => {};
  title: (title: string) => void = () => {};
  bell: () => void = () => {};
  mounted: HTMLElement | null = null;
  looks = 0;
  reader = false;
  labelled = "";
  focused = 0;
  copies = 0;
  selection = false;
  selectedAll = 0;
  cleared = 0;
  write(bytes: Uint8Array, done: () => void): void {
    this.written.push({ bytes, done });
  }
  separator(): void {
    this.separators += 1;
  }
  disable(): void {
    this.enabled = false;
  }
  enable(): void {
    this.enabled = true;
  }
  size(): Size {
    return { cols: this.cols, rows: this.rows };
  }
  onInput(listener: (text: string) => void): void {
    this.input = listener;
  }
  onBinary(listener: (bytes: Uint8Array) => void): void {
    this.binary = listener;
  }
  onResize(listener: (size: Size) => void): void {
    this.resize = listener;
  }
  onTitle(listener: (title: string) => void): void {
    this.title = listener;
  }
  onBell(listener: () => void): void {
    this.bell = listener;
  }
  dispose(): void {
    this.disposed = true;
  }
  mount(host: HTMLElement): void {
    this.mounted = host;
  }
  unmount(): void {
    this.mounted = null;
  }
  fit(): void {}
  applyLook(): void {
    this.looks += 1;
  }
  setScreenReaderMode(on: boolean): void {
    this.reader = on;
  }
  label(text: string): void {
    this.labelled = text;
  }
  focus(): void {
    this.focused += 1;
  }
  copy(): boolean {
    this.copies += 1;
    return this.selection;
  }
  selectAll(): void {
    this.selectedAll += 1;
  }
  clear(): void {
    this.cleared += 1;
  }
  hasSelection(): boolean {
    return this.selection;
  }
}

/** What the real emulator needs of a browser and jsdom does not have. */
export function emulatorDom(): void {
  Object.defineProperty(globalThis, "matchMedia", {
    configurable: true,
    writable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      addEventListener() {},
      removeEventListener() {},
      addListener() {},
      removeListener() {},
    }),
  });
  HTMLCanvasElement.prototype.getContext = () => null;
}
