// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TerminalSession } from "#lib/terminal/session.svelte.ts";
import { FakeScreen } from "#lib/testing/fake-terminal.ts";
import TerminalView from "./TerminalView.svelte";

let schemeListeners: (() => void)[] = [];
beforeEach(() => {
  schemeListeners = [];
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener: (_: string, l: () => void) => schemeListeners.push(l),
    removeEventListener: (_: string, l: () => void) => {
      schemeListeners = schemeListeners.filter((x) => x !== l);
    },
  }));
});
afterEach(() => {
  vi.unstubAllGlobals();
  cleanup();
});

function make(
  over: Partial<Parameters<typeof render<typeof TerminalView>>[1]> = {},
) {
  const fake = new FakeScreen();
  const session = new TerminalSession("w", 2, fake);
  const props = {
    session,
    workspaceName: "web",
    onReady: vi.fn(),
    onLeave: vi.fn(),
    ...over,
  };
  const view = render(TerminalView, props as never);
  return { fake, session, props, view };
}

describe("TerminalView", () => {
  it("mounts the emulator, labels it and reports its size once", () => {
    const { fake, props } = make();
    expect(fake.mounted).toBe(
      screen.getByRole("group", { name: "Terminal 2, web" }),
    );
    expect(fake.labelled).toBe("Terminal 2, web");
    expect(fake.looks).toBe(1);
    expect(props.onReady).toHaveBeenCalledWith({ cols: 80, rows: 24 });
    expect(fake.focused).toBe(0);
  });

  it("does not report a size for a session that already runs, and focuses a new one on request", () => {
    const fake = new FakeScreen();
    const session = new TerminalSession("w", 1, fake);
    session.state = "live";
    const onReady = vi.fn();
    render(TerminalView, {
      session,
      workspaceName: "web",
      focusOnOpen: true,
      screenReader: true,
      onReady,
      onLeave: () => {},
    });
    expect(onReady).not.toHaveBeenCalled();
    expect(fake.focused).toBe(1);
    expect(fake.reader).toBe(true);
  });

  it("follows the theme: a change on the root element or of the colour scheme applies the look again", async () => {
    const { fake } = make();
    document.documentElement.setAttribute("data-theme", "dark");
    await waitFor(() => expect(fake.looks).toBe(2));
    schemeListeners.forEach((l) => l());
    expect(fake.looks).toBe(3);
    document.documentElement.removeAttribute("data-theme");
  });

  it("shows the address a hovered link goes to, and F6 leaves", () => {
    const { fake, props } = make();
    fake.onHover("https://example.com/");
    return waitFor(() =>
      expect(screen.getByText("https://example.com/")).toBeInTheDocument(),
    ).then(() => {
      fake.onLeave();
      expect(props.onLeave).toHaveBeenCalled();
      fake.onHover(null);
    });
  });

  it("fits the emulator again when its area changes", () => {
    let report: () => void = () => {};
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(callback: () => void) {
          report = callback;
        }
        observe() {}
        disconnect() {}
      },
    );
    const { fake } = make();
    const fit = vi.spyOn(fake, "fit");
    report();
    expect(fit).toHaveBeenCalledTimes(1);
  });

  it("gives the emulator back when it goes away", () => {
    const { fake, view } = make();
    view.unmount();
    expect(fake.mounted).toBeNull();
    expect(schemeListeners).toEqual([]);
    fake.onHover("x");
    fake.onLeave();
  });

  describe("the right-click menu", () => {
    async function open(selection: boolean) {
      const made = make();
      made.fake.selection = selection;
      await fireEvent.contextMenu(
        screen.getByRole("group", { name: "Terminal 2, web" }),
        {
          clientX: 30,
          clientY: 20,
        },
      );
      return made;
    }

    it("offers Copy, Select all and Clear, and focuses the first that works", async () => {
      await open(true);
      const menu = screen.getByRole("menu", { name: "Terminal actions" });
      expect(menu).toBeInTheDocument();
      await waitFor(() =>
        expect(screen.getByRole("menuitem", { name: "Copy" })).toHaveFocus(),
      );
      expect(
        screen.getByRole("menuitem", { name: "Select all" }),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("menuitem", { name: "Clear" }),
      ).toBeInTheDocument();
    });

    it("copies the selection and gives focus back to the terminal", async () => {
      const { fake } = await open(true);
      await fireEvent.click(screen.getByRole("menuitem", { name: "Copy" }));
      expect(fake.copies).toBe(1);
      expect(fake.focused).toBe(1);
      expect(screen.queryByRole("menu")).toBeNull();
    });

    it("turns Copy off with nothing selected, and selects all or clears on request", async () => {
      const { fake } = await open(false);
      expect(screen.getByRole("menuitem", { name: "Copy" })).toBeDisabled();
      await waitFor(() =>
        expect(
          screen.getByRole("menuitem", { name: "Select all" }),
        ).toHaveFocus(),
      );
      await fireEvent.click(
        screen.getByRole("menuitem", { name: "Select all" }),
      );
      expect(fake.selectedAll).toBe(1);
      await fireEvent.contextMenu(
        screen.getByRole("group", { name: "Terminal 2, web" }),
      );
      await fireEvent.click(screen.getByRole("menuitem", { name: "Clear" }));
      expect(fake.cleared).toBe(1);
    });

    it("moves with the arrows, wraps, and closes on Escape", async () => {
      await open(true);
      const menu = screen.getByRole("menu");
      const copy = screen.getByRole("menuitem", { name: "Copy" });
      await waitFor(() => expect(copy).toHaveFocus());
      await fireEvent.keyDown(menu, { key: "ArrowDown" });
      expect(
        screen.getByRole("menuitem", { name: "Select all" }),
      ).toHaveFocus();
      await fireEvent.keyDown(menu, { key: "ArrowUp" });
      await fireEvent.keyDown(menu, { key: "ArrowUp" });
      expect(screen.getByRole("menuitem", { name: "Clear" })).toHaveFocus();
      await fireEvent.keyDown(menu, { key: "a" });
      expect(menu).toBeInTheDocument();
      await fireEvent.keyDown(menu, { key: "Escape" });
      expect(screen.queryByRole("menu")).toBeNull();
    });

    it("closes when focus leaves it, without taking focus back", async () => {
      const { fake } = await open(true);
      const menu = screen.getByRole("menu");
      await fireEvent.focusOut(menu, { relatedTarget: document.body });
      expect(screen.queryByRole("menu")).toBeNull();
      expect(fake.focused).toBe(0);
    });

    it("stays open while focus moves between its own items", async () => {
      await open(true);
      const menu = screen.getByRole("menu");
      await fireEvent.focusOut(menu, {
        relatedTarget: screen.getByRole("menuitem", { name: "Clear" }),
      });
      expect(screen.getByRole("menu")).toBeInTheDocument();
    });
  });
});
