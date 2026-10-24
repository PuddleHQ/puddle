// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { NoticeCenter } from "#lib/notify/notices.svelte.ts";
import Notices from "./Notices.svelte";

afterEach(cleanup);

function setup(count = 0) {
  const center = new NoticeCenter();
  for (let i = 1; i <= count; i += 1) {
    center.add({
      key: `k${i}`,
      tone: i % 2 ? "warning" : "info",
      title: `Title ${i}`,
      ...(i === 1
        ? { detail: "More words", link: { href: "/x", label: "Go there" } }
        : {}),
    });
  }
  render(Notices, { center });
  return center;
}

describe("Notices", () => {
  it("keeps an empty polite live region, so a notice that arrives is announced", async () => {
    const center = setup();
    const region = screen.getByRole("region", { name: "Notices" });
    expect(region.querySelector("[aria-live=polite]")).not.toBeNull();
    center.add({ key: "a", tone: "info", title: "Hello" });
    expect(await screen.findByText("Hello")).toBeInTheDocument();
    expect(document.activeElement).toBe(document.body);
  });

  it("shows title, detail and link as text, newest first", () => {
    setup(2);
    expect(screen.getByText("More words")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Go there" })).toHaveAttribute(
      "href",
      "/x",
    );
    const titles = screen.getAllByText(/^Title/).map((e) => e.textContent);
    expect(titles).toEqual(["Title 2", "Title 1"]);
  });

  it("never draws a title as markup", () => {
    const center = setup();
    center.add({
      key: "a",
      tone: "info",
      title: "<img src=x onerror=alert(1)>",
    });
    expect(document.querySelector("img")).toBeNull();
  });

  it("dismisses one notice and moves focus to a neighbour, or to the page when none is left", async () => {
    document.body.insertAdjacentHTML(
      "beforeend",
      '<main id="main" tabindex="-1"></main>',
    );
    const center = setup(2);
    const buttons = screen.getAllByRole("button", { name: /^Dismiss notice/ });
    expect(buttons[0]).toHaveAccessibleName("Dismiss notice: Title 2");
    await fireEvent.click(buttons[0]!);
    expect(center.items.map((n) => n.key)).toEqual(["k1"]);
    expect(document.activeElement).toBe(
      screen.getByRole("button", { name: "Dismiss notice: Title 1" }),
    );
    await fireEvent.click(document.activeElement!);
    expect(center.items).toEqual([]);
    expect(document.activeElement).toBe(document.getElementById("main"));
    document.getElementById("main")?.remove();
  });

  it("folds a long list and offers to show all or dismiss all", async () => {
    const center = setup(5);
    expect(screen.getAllByText(/^Title/)).toHaveLength(3);
    const toggle = screen.getByRole("button", { name: "Show all 5 notices" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    await fireEvent.click(toggle);
    expect(screen.getAllByText(/^Title/)).toHaveLength(5);
    await fireEvent.click(screen.getByRole("button", { name: "Show fewer" }));
    expect(screen.getAllByText(/^Title/)).toHaveLength(3);
    await fireEvent.click(screen.getByRole("button", { name: "Dismiss all" }));
    expect(center.items).toEqual([]);
  });

  it("runs a notice's action on a click, and only then", async () => {
    const center = setup();
    const run = vi.fn();
    center.add({
      key: "a",
      tone: "warning",
      title: "A push was refused",
      action: { label: "Allow push", run },
      link: { href: "/git", label: "Git tab" },
    });
    expect(run).not.toHaveBeenCalled();
    await fireEvent.click(
      await screen.findByRole("button", { name: "Allow push" }),
    );
    expect(run).toHaveBeenCalledOnce();
    expect(screen.getByRole("link", { name: "Git tab" })).toBeInTheDocument();
  });
  describe("an action that asks first", () => {
    const confirm = {
      title: "Forget the login?",
      summary: "Puddle deletes the saved sign-in.",
      detail: "The tool asks you to sign in again.",
      confirmLabel: "Forget login",
    };

    /** The dialog hides the page behind it from the accessibility tree, so look inside it. */
    const confirmButton = async () =>
      within(await screen.findByRole("alertdialog")).getByRole("button", {
        name: "Forget login",
      });

    function withAction(run: () => Promise<void>) {
      const center = setup();
      center.add({
        key: "a",
        tone: "warning",
        title: "A login can't be used",
        action: { label: "Forget login", run, confirm },
      });
      return center;
    }

    it("opens a dialog on the click and runs nothing until it is confirmed", async () => {
      const run = vi.fn(() => Promise.resolve());
      withAction(run);
      await fireEvent.click(
        await screen.findByRole("button", { name: "Forget login" }),
      );
      const dialog = await screen.findByRole("alertdialog");
      expect(dialog).toHaveAccessibleName("Forget the login?");
      expect(dialog).toHaveTextContent("Puddle deletes the saved sign-in.");
      expect(dialog).toHaveTextContent("The tool asks you to sign in again.");
      expect(run).not.toHaveBeenCalled();
      await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
      expect(run).not.toHaveBeenCalled();
    });

    it("closes after a confirmed run and puts the focus on the page when the notice went with it", async () => {
      document.body.insertAdjacentHTML(
        "beforeend",
        '<main id="main" tabindex="-1"></main>',
      );
      const run = vi.fn(() => {
        center.resolve("a");
        return Promise.resolve();
      });
      const center: NoticeCenter = withAction(run);
      await fireEvent.click(
        await screen.findByRole("button", { name: "Forget login" }),
      );
      await fireEvent.click(await confirmButton());
      expect(run).toHaveBeenCalledOnce();
      await vi.waitFor(() =>
        expect(screen.queryByRole("alertdialog")).toBeNull(),
      );
      expect(document.activeElement).toBe(document.getElementById("main"));
      document.getElementById("main")?.remove();
    });

    it("keeps the dialog open and shows why when the run fails, then lets it be tried again", async () => {
      const run = vi
        .fn<() => Promise<void>>()
        .mockRejectedValueOnce(new Error("The store refused. Unlock it."))
        .mockResolvedValueOnce(undefined);
      withAction(run);
      await fireEvent.click(
        await screen.findByRole("button", { name: "Forget login" }),
      );
      await fireEvent.click(await confirmButton());
      expect(await screen.findByRole("alert")).toHaveTextContent(
        "The store refused. Unlock it.",
      );
      expect(screen.getByRole("alertdialog")).toBeInTheDocument();
      expect(await confirmButton()).toBeEnabled();
      await fireEvent.click(await confirmButton());
      await vi.waitFor(() =>
        expect(screen.queryByRole("alertdialog")).toBeNull(),
      );
      expect(run).toHaveBeenCalledTimes(2);
    });

    it("shows a failure that is not an Error as text", async () => {
      withAction(() => Promise.reject("plain text failure"));
      await fireEvent.click(
        await screen.findByRole("button", { name: "Forget login" }),
      );
      await fireEvent.click(await confirmButton());
      expect(await screen.findByRole("alert")).toHaveTextContent(
        "plain text failure",
      );
    });
  });
});
