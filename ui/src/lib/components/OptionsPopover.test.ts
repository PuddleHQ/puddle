// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Choice, Target } from "#lib/decision/model.ts";
import OptionsPopover from "./OptionsPopover.svelte";

afterEach(cleanup);

const target: Target = {
  host: "api.example.com",
  registrableDomain: "example.com",
};

/** jsdom has no layout, so floating-ui never marks the popover positioned; show it by hand. */
async function show(): Promise<HTMLElement> {
  const dialog = await screen.findByRole("dialog", { hidden: true });
  const wrapper = dialog.parentElement;
  if (wrapper) wrapper.style.visibility = "visible";
  return dialog;
}

function mount(props: Record<string, unknown> = {}) {
  const onDecide = vi.fn<(choice: Choice) => void>();
  const onClose = vi.fn();
  const anchor = document.createElement("button");
  document.body.append(anchor);
  render(OptionsPopover, {
    props: {
      open: true,
      target,
      workspace: "demo",
      anchor,
      onDecide,
      onClose,
      ...props,
    },
  } as never);
  return { onDecide, onClose, anchor };
}

// The first popover in a file builds bits-ui's, floating-ui's and the role queries' tables: unloaded
// about 90 ms on top of the first test, on a busy host seconds, over its 5000 ms. Pay it here,
// outside any test's clock.
mount({ sets: [{ id: 4, name: "Warm-up", everywhere: false }] });
const warm = await show();
await fireEvent.click(
  within(warm).getByRole("radio", { name: /Into rule set Warm-up/ }),
);
cleanup();

describe("OptionsPopover", () => {
  it("offers your rule sets that are on here, and sends the one chosen", async () => {
    const { onDecide } = mount({
      sets: [
        { id: 4, name: "Client X", everywhere: true },
        { id: 5, name: "Local", everywhere: false },
      ],
    });
    const dialog = await show();
    expect(
      within(dialog).getByRole("radio", { name: /Into rule set Client X/ }),
    ).toHaveAccessibleName(/on everywhere; asks/);
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /Into rule set Client X/ }),
    );
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /^Only demo/ }),
    );
    expect(
      within(dialog).getByRole("radio", { name: /^Only demo/ }),
    ).toBeChecked();
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /Into rule set Local/ }),
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: /Allow/ }),
    );
    expect(onDecide).toHaveBeenCalledWith({
      effect: "allow",
      scope: "workspace",
      ruleSet: { id: 5, name: "Local", everywhere: false },
      match: "exact",
      durationSecs: null,
    });
  });

  it("refuses a rule set that was deleted while the popover was open", async () => {
    const onDecide = vi.fn();
    const anchor = document.createElement("button");
    document.body.append(anchor);
    const props = {
      open: true,
      target,
      workspace: "demo",
      anchor,
      onDecide,
      onClose: vi.fn(),
      sets: [{ id: 4, name: "Client X", everywhere: true }],
    };
    const view = render(OptionsPopover, { props } as never);
    const dialog = await show();
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /Into rule set Client X/ }),
    );
    await view.rerender({ ...props, sets: [] } as never);
    await fireEvent.click(
      within(dialog).getByRole("button", { name: /Allow/ }),
    );
    expect(onDecide).not.toHaveBeenCalled();
    expect(within(dialog).getByRole("alert")).toHaveTextContent(
      "That rule set no longer exists. Choose again.",
    );
  });

  it("is a named dialog that starts at the narrowest choice and lists the durations", async () => {
    mount();
    const dialog = await show();
    expect(dialog).toHaveAccessibleName("Choices for api.example.com");
    expect(
      within(dialog).getByRole("radio", { name: /^Only demo/ }),
    ).toBeChecked();
    expect(
      within(dialog).getByRole("radio", { name: /^Only api\.example\.com/ }),
    ).toBeChecked();
    const select = within(dialog).getByRole("combobox", { name: "How long" });
    expect(select).toHaveValue("0");
    expect(
      within(select)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Permanently", "1 hour", "8 hours", "1 day", "7 days"]);
  });

  it("sends every workspace, the registrable domain as a suffix and a duration", async () => {
    const { onDecide } = mount();
    const dialog = await show();
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /^Every workspace/ }),
    );
    await fireEvent.click(
      within(dialog).getByRole("radio", {
        name: /^Everything under example\.com/,
      }),
    );
    await fireEvent.change(within(dialog).getByRole("combobox"), {
      target: { value: "28800" },
    });
    await fireEvent.click(within(dialog).getByRole("button", { name: /Deny/ }));
    expect(onDecide).toHaveBeenCalledWith({
      effect: "deny",
      scope: "global",
      ruleSet: null,
      match: "suffix",
      durationSecs: 28_800,
    });
  });

  it("sends an exact match when the user goes back from the subdomains", async () => {
    const { onDecide } = mount();
    const dialog = await show();
    await fireEvent.click(
      within(dialog).getByRole("radio", {
        name: /^Everything under example\.com/,
      }),
    );
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /^Only api\.example\.com/ }),
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: /Allow/ }),
    );
    expect(onDecide).toHaveBeenCalledWith(
      expect.objectContaining({ effect: "allow", match: "exact" }),
    );
  });

  it("starts from the narrowest choice every time it opens", async () => {
    mount();
    let dialog = await show();
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /^Every workspace/ }),
    );
    cleanup();
    mount();
    dialog = await show();
    expect(
      within(dialog).getByRole("radio", { name: /^Only demo/ }),
    ).toBeChecked();
  });

  it("closes with Cancel and Escape without deciding, and focuses the anchor again", async () => {
    const { onDecide, onClose, anchor } = mount();
    const dialog = await show();
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await vi.waitFor(() => expect(onClose).toHaveBeenCalled());
    await vi.waitFor(() => expect(anchor).toHaveFocus());
    onClose.mockClear();
    await fireEvent.keyDown(dialog, { key: "Escape" });
    expect(onDecide).not.toHaveBeenCalled();
  });

  const press = (on: Element) =>
    fireEvent.pointerDown(on, {
      button: 0,
      pointerType: "mouse",
      clientX: 500,
      clientY: 500,
    });

  it("closes without deciding when the user presses outside it", async () => {
    const { onDecide, onClose } = mount();
    await show();
    await press(document.body);
    await vi.waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(onDecide).not.toHaveBeenCalled();
  });

  it("stays open when the press lands on its own chevron, which toggles it", async () => {
    const { onClose, anchor } = mount();
    await show();
    await press(anchor);
    await new Promise((done) => setTimeout(done, 50));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { hidden: true })).toBeInTheDocument();
  });

  it("opened again, keeps focus on a choice focused at once; the anchor gets it back only on close", async () => {
    const anchor = document.createElement("button");
    document.body.append(anchor);
    const props = {
      target,
      workspace: "demo",
      anchor,
      onDecide: vi.fn(),
      onClose: vi.fn(),
    };
    const view = render(OptionsPopover, {
      props: { ...props, open: true },
    } as never);
    await show();
    await view.rerender({ ...props, open: false });
    await vi.waitFor(() => expect(anchor).toHaveFocus());
    // Opened again, the popover settles a frame later (its opening transition ends). Focus that
    // moved in before then stays where it was put.
    await view.rerender({ ...props, open: true });
    const every = within(await show()).getByRole("radio", {
      name: /^Every workspace/,
    });
    every.focus();
    for (let frame = 0; frame < 3; frame += 1) {
      await new Promise((done) => requestAnimationFrame(done));
    }
    expect(every).toHaveFocus();
  });

  it("offers no subdomain choice for the registrable domain itself or an exact-only request", async () => {
    mount({
      target: { host: "example.com", registrableDomain: "example.com" },
    });
    expect(
      within(await show()).queryByRole("radio", { name: /Everything under/ }),
    ).toBeNull();
    cleanup();
    mount({ exactOnly: true });
    expect(
      within(await show()).queryByRole("radio", { name: /Everything under/ }),
    ).toBeNull();
  });

  it("renders nothing while closed", () => {
    mount({ open: false });
    expect(screen.queryByRole("dialog", { hidden: true })).toBeNull();
  });
});
