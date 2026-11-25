// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Choice, Target } from "#lib/decision/model.ts";
import DecisionControl from "./DecisionControl.svelte";

afterEach(cleanup);

const target: Target = {
  host: "api.example.com",
  registrableDomain: "example.com",
};

function mount(props: Record<string, unknown> = {}) {
  const onDecide = vi.fn<(choice: Choice) => void>();
  const onMore = vi.fn<(anchor: HTMLElement) => void>();
  render(DecisionControl, {
    props: { target, workspace: "demo", onDecide, onMore, ...props },
  } as never);
  return { onDecide, onMore };
}

describe("the one-click buttons", () => {
  it("Allow and Deny decide for this workspace, the exact host, permanently", async () => {
    const { onDecide } = mount();
    await fireEvent.click(
      screen.getByRole("button", { name: "Allow api.example.com for demo" }),
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Deny api.example.com for demo" }),
    );
    const narrow = {
      scope: "workspace",
      ruleSet: null,
      match: "exact",
      durationSecs: null,
    };
    expect(onDecide).toHaveBeenNthCalledWith(1, { effect: "allow", ...narrow });
    expect(onDecide).toHaveBeenNthCalledWith(2, { effect: "deny", ...narrow });
  });

  it("the more-choices button hands its own element to the parent and shows whether the options are open", async () => {
    const { onMore } = mount();
    const more = screen.getByRole("button", {
      name: "More choices for api.example.com",
    });
    expect(more).toHaveAttribute("aria-haspopup", "dialog");
    expect(more).toHaveAttribute("aria-expanded", "false");
    await fireEvent.click(more);
    expect(onMore).toHaveBeenCalledWith(more);
    cleanup();
    mount({ optionsOpen: true });
    expect(
      screen.getByRole("button", { name: /^More choices/ }),
    ).toHaveAttribute("aria-expanded", "true");
  });

  it("shows Allow, then Deny, then a neutral more-choices button that belongs to neither", () => {
    mount();
    const names = screen
      .getAllByRole("button")
      .map((b) => b.getAttribute("aria-label"));
    expect(names).toEqual([
      "Allow api.example.com for demo",
      "Deny api.example.com for demo",
      "More choices for api.example.com",
    ]);
    const more = screen.getByRole("button", { name: /^More choices/ });
    // Green is Allow's and red is Deny's; the menu that holds both kinds of choice is grey.
    expect(more).not.toHaveClass("allow");
    expect(more).not.toHaveClass("deny");
    const allow = screen.getByRole("button", { name: /^Allow/ });
    expect(allow.parentElement).toBe(more.parentElement);
  });

  it("offers only Deny when the request can't be approved yet", () => {
    mount({ denyOnly: true });
    expect(screen.queryByRole("button", { name: /^Allow/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /More choices/ })).toBeNull();
    expect(screen.getByRole("button", { name: /^Deny/ })).toBeInTheDocument();
  });

  it("keeps the word workspace in the labels, never sandbox", () => {
    mount();
    expect(document.body.textContent).not.toMatch(/sandbox/i);
    for (const button of screen.getAllByRole("button")) {
      expect(button.getAttribute("aria-label") ?? "").not.toMatch(/sandbox/i);
    }
  });
});

describe("Ctrl+click", () => {
  const global = {
    scope: "global",
    ruleSet: null,
    match: "exact",
    durationSecs: null,
  };

  it("decides for every workspace on Allow and Deny, with Ctrl or Cmd, without the options", async () => {
    const { onDecide, onMore } = mount();
    await fireEvent.click(
      screen.getByRole("button", { name: "Allow api.example.com for demo" }),
      { ctrlKey: true },
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Deny api.example.com for demo" }),
      { metaKey: true },
    );
    expect(onDecide).toHaveBeenNthCalledWith(1, { effect: "allow", ...global });
    expect(onDecide).toHaveBeenNthCalledWith(2, { effect: "deny", ...global });
    expect(onMore).not.toHaveBeenCalled();
  });

  it("puts no native title on any button, so no unstyled tooltip appears; the shortcuts popover explains Ctrl+click", () => {
    mount();
    const buttons = screen.getAllByRole("button");
    expect(buttons).toHaveLength(3);
    for (const button of buttons) {
      expect(button).not.toHaveAttribute("title");
    }
  });
});
