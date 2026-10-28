// SPDX-License-Identifier: GPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import ExpiryDialog from "./ExpiryDialog.svelte";

afterEach(cleanup);

describe("the Change expiry dialog", () => {
  it("saves nothing and stays open when there is no rule to change", async () => {
    const onSave = vi.fn(async () => null);
    render(ExpiryDialog, {
      props: { open: true, rule: null, now: () => 0, onSave },
    } as never);
    await screen.findByRole("dialog", { name: "Change expiry" });
    await fireEvent.change(screen.getByLabelText("Ends"), {
      target: { value: "0" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(onSave).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });
});
