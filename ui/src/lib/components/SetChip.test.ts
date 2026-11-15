// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it } from "vitest";
import { inkOn, solid } from "#lib/rules/fill.ts";
import SetChip from "./SetChip.svelte";

afterEach(cleanup);

describe("a rule set's colour beside its name", () => {
  it("paints a chip with the colour and the ink that reads on it", () => {
    render(SetChip, {
      props: { name: "Client X", fill: solid("#0a3a50"), variant: "chip" },
    } as never);
    const chip = screen.getByText("Client X");
    expect(chip).toHaveStyle(
      `--set-fill: #0a3a50; --set-ink: ${inkOn("#0a3a50")}`,
    );
    expect(inkOn("#0a3a50")).toBe("#ffffff");
  });

  it("puts a decorative dot before the name, which stays text", () => {
    const { container } = render(SetChip, {
      props: { name: "Client X", fill: solid("#d98a1f") },
    } as never);
    expect(screen.getByText("Client X")).toBeVisible();
    const dot = container.querySelector(".dot");
    expect(dot).toHaveAttribute("aria-hidden", "true");
    expect(dot?.parentElement).toHaveStyle("--set-fill: #d98a1f");
  });

  it("shows only the name when the record has no colour (an older service)", () => {
    const { container } = render(SetChip, {
      props: { name: "Old", fill: undefined, variant: "chip" },
    } as never);
    expect(screen.getByText("Old")).toBeVisible();
    expect(container.querySelector(".dot, .set-chip")).toBeNull();
  });
});
