// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import ThreeWaySwitch from "./ThreeWaySwitch.svelte";

afterEach(cleanup);

describe("the three-way switch", () => {
  it("highlights the current segment and says what Follow means now", () => {
    render(ThreeWaySwitch, {
      props: { label: "x", value: null, inherited: true, onChange: vi.fn() },
    });
    const pressed = screen
      .getAllByRole("button")
      .filter((b) => b.getAttribute("aria-pressed") === "true");
    expect(pressed.map((b) => b.textContent)).toEqual(["Follow (on)"]);
  });

  it("reports a change, and nothing when the current segment is clicked", async () => {
    const onChange = vi.fn();
    render(ThreeWaySwitch, {
      props: { label: "x", value: true, inherited: false, onChange },
    });
    await fireEvent.click(screen.getByRole("button", { name: "On" }));
    expect(onChange).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole("button", { name: "Off" }));
    expect(onChange).toHaveBeenCalledWith(false);
    await fireEvent.click(screen.getByRole("button", { name: "Follow (off)" }));
    expect(onChange).toHaveBeenLastCalledWith(null);
  });
});
