// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeAll, describe, expect, it, vi } from "vitest";
import { emulatorDom } from "#lib/testing/fake-terminal.ts";
import type { PortEntry } from "#lib/workspaces/ports.ts";
import { XtermScreen } from "./xterm-screen.ts";

beforeAll(emulatorDom);

function make(forwards: PortEntry[] = []) {
  const opened: string[] = [];
  const screen = new XtermScreen({
    forwards: () => forwards,
    open: (url) => opened.push(url),
  });
  const host = document.createElement("div");
  document.body.append(host);
  screen.mount(host);
  return { screen, host, opened };
}

describe("the emulator", () => {
  it("draws bytes and says when they are drawn", async () => {
    const { screen } = make();
    await new Promise<void>((done) =>
      screen.write(new TextEncoder().encode("hi\r\n"), done),
    );
    expect(screen.term.buffer.active.getLine(0)?.translateToString(true)).toBe(
      "hi",
    );
  });

  it("keeps 10,000 lines and asks for contrast of 4.5", () => {
    const { screen } = make();
    expect(screen.term.options.scrollback).toBe(10_000);
    expect(screen.term.options.minimumContrastRatio).toBe(4.5);
    expect(screen.size()).toEqual({ cols: 80, rows: 24 });
  });

  it("moves between hosts and out of the page, keeping its contents", async () => {
    const { screen, host } = make();
    await new Promise<void>((done) =>
      screen.write(new TextEncoder().encode("kept"), done),
    );
    screen.unmount();
    expect(host.querySelector(".xterm")).toBeNull();
    const other = document.createElement("div");
    document.body.append(other);
    screen.mount(other);
    expect(other.querySelector(".xterm")).not.toBeNull();
    screen.mount(other);
    expect(other.querySelectorAll(".xterm")).toHaveLength(1);
    expect(screen.term.buffer.active.getLine(0)?.translateToString(true)).toBe(
      "kept",
    );
  });

  it("writes a separator and stops and resumes reading keys", async () => {
    const { screen } = make();
    screen.separator();
    screen.disable();
    expect(screen.term.options.disableStdin).toBe(true);
    screen.enable();
    expect(screen.term.options.disableStdin).toBe(false);
    await new Promise<void>((done) => screen.write(new Uint8Array(), done));
    expect(
      screen.term.buffer.active.getLine(1)?.translateToString(true),
    ).toMatch(/^-+$/);
  });

  it("tells the session what is typed, binary reports, the title, the bell and the size", async () => {
    const { screen } = make();
    const text: string[] = [];
    const binary: Uint8Array[] = [];
    const titles: string[] = [];
    const sizes: unknown[] = [];
    let bells = 0;
    screen.onInput((t) => text.push(t));
    screen.onBinary((b) => binary.push(b));
    screen.onTitle((t) => titles.push(t));
    screen.onBell(() => (bells += 1));
    screen.onResize((s) => sizes.push(s));
    screen.term.input("ls\r", true);
    // Only a mouse report beyond column 95 makes one, and there is no layout here to click on.
    (
      screen.term as unknown as {
        _core: { coreService: { triggerBinaryEvent(data: string): void } };
      }
    )._core.coreService.triggerBinaryEvent(String.fromCharCode(255));
    await new Promise<void>((done) =>
      screen.write(
        new TextEncoder().encode("\u001b]0;my title\u0007\u0007"),
        done,
      ),
    );
    screen.term.resize(100, 30);
    expect(text).toEqual(["ls\r"]);
    expect(binary).toEqual([Uint8Array.of(255)]);
    expect(titles).toEqual(["my title"]);
    expect(bells).toBe(1);
    expect(sizes).toEqual([{ cols: 100, rows: 30 }]);
    screen.dispose();
  });

  it("applies the UI's look, a label and screen reader mode", () => {
    const { screen, host } = make();
    screen.applyLook(host);
    expect(screen.term.options.theme?.background).toMatch(/^#[0-9a-f]{6}$/);
    expect(screen.term.options.theme?.red).toMatch(/^#[0-9a-f]{6}$/);
    screen.label("Terminal 1, demo");
    expect(screen.term.textarea?.getAttribute("aria-label")).toBe(
      "Terminal 1, demo",
    );
    screen.setScreenReaderMode(true);
    expect(screen.term.options.screenReaderMode).toBe(true);
    screen.focus();
    expect(document.activeElement).toBe(screen.term.textarea);
  });

  it("selects all and clears, and copies only a selection", async () => {
    const { screen } = make();
    expect(screen.hasSelection()).toBe(false);
    const exec = vi.fn(() => true);
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: exec,
    });
    expect(screen.copy()).toBe(false);
    expect(exec).not.toHaveBeenCalled();
    await new Promise<void>((done) =>
      screen.write(new TextEncoder().encode("a\r\nb\r\nc"), done),
    );
    screen.selectAll();
    expect(screen.hasSelection()).toBe(true);
    expect(screen.copy()).toBe(true);
    expect(exec).toHaveBeenCalledWith("copy");
    screen.clear();
    expect(screen.term.buffer.active.getLine(0)?.translateToString(true)).toBe(
      "c",
    );
  });
});

/** Presses a key in the emulator's input as the browser would. */
function press(screen: XtermScreen, init: KeyboardEventInit, type = "keydown") {
  const event = new KeyboardEvent(type, {
    cancelable: true,
    bubbles: true,
    ...init,
  });
  screen.term.textarea?.dispatchEvent(event);
  return event;
}

describe("keys", () => {
  it("copies on Ctrl+Shift+C without sending it, and leaves paste to the paste event", async () => {
    const { screen } = make();
    const sent: string[] = [];
    screen.onInput((t) => sent.push(t));
    const exec = vi.fn(() => true);
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: exec,
    });
    await new Promise<void>((done) =>
      screen.write(new TextEncoder().encode("abc"), done),
    );
    screen.selectAll();
    const copy = press(screen, { key: "C", ctrlKey: true, shiftKey: true });
    expect(copy.defaultPrevented).toBe(true);
    expect(exec).toHaveBeenCalledWith("copy");
    const paste = press(screen, { key: "V", ctrlKey: true, shiftKey: true });
    expect(paste.defaultPrevented).toBe(false);
    expect(sent).toEqual([]);
  });

  it("leaves the terminal on F6", () => {
    const { screen } = make();
    let left = 0;
    screen.onLeave = () => (left += 1);
    const sent: string[] = [];
    screen.onInput((t) => sent.push(t));
    const f6 = press(screen, { key: "F6" });
    expect(left).toBe(1);
    expect(f6.defaultPrevented).toBe(true);
    expect(sent).toEqual([]);
  });

  it("hands Ctrl+R to the shell and cancels the browser's default, ", () => {
    const { screen } = make();
    const sent: string[] = [];
    screen.onInput((t) => sent.push(t));
    const reload = press(screen, {
      key: "r",
      ctrlKey: true,
      code: "KeyR",
      keyCode: 82,
    });
    expect(reload.defaultPrevented).toBe(true);
    expect(sent).toEqual(["\u0012"]);
    press(screen, { key: "r", ctrlKey: true }, "keyup");
    expect(sent).toEqual(["\u0012"]);
  });

  it("does not cancel a character typed with AltGr", () => {
    const { screen } = make();
    const event = new KeyboardEvent("keydown", {
      key: "@",
      ctrlKey: true,
      altKey: true,
      cancelable: true,
      bubbles: true,
      modifierAltGraph: true,
    });
    screen.term.textarea?.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
  });
});

describe("links", () => {
  it("opens a web address on Ctrl+click and not on a plain click", () => {
    const { screen, opened } = make();
    const handler = screen.term.options.linkHandler;
    handler?.activate(
      new MouseEvent("click"),
      "https://example.com/x",
      {} as never,
    );
    expect(opened).toEqual([]);
    handler?.activate(
      new MouseEvent("click", { ctrlKey: true }),
      "https://example.com/x",
      {} as never,
    );
    expect(opened).toEqual(["https://example.com/x"]);
  });

  it("hovering or leaving a link before the page listens does nothing", () => {
    const { screen } = make();
    const handler = screen.term.options.linkHandler;
    handler?.hover?.(new MouseEvent("mousemove"), "https://a.b/", {} as never);
    handler?.leave?.(new MouseEvent("mouseout"), "https://a.b/", {} as never);
    screen.onLeave();
  });

  it("opens nothing for another scheme or an unforwarded local port, and says what to do", () => {
    const { screen, opened } = make();
    const handler = screen.term.options.linkHandler;
    const ctrl = new MouseEvent("click", { ctrlKey: true });
    handler?.activate(ctrl, "file:///etc/passwd", {} as never);
    handler?.activate(ctrl, "http://localhost:3000", {} as never);
    expect(opened).toEqual([]);
    const hovers: (string | null)[] = [];
    screen.onHover = (t) => hovers.push(t);
    handler?.hover?.(
      new MouseEvent("mousemove"),
      "http://localhost:3000",
      {} as never,
    );
    handler?.hover?.(new MouseEvent("mousemove"), "https://a.b/", {} as never);
    handler?.leave?.(new MouseEvent("mouseout"), "https://a.b/", {} as never);
    expect(hovers).toEqual([
      "http://localhost:3000/ (forward port 3000 on the Ports tab to open it)",
      "https://a.b/",
      null,
    ]);
  });
});
