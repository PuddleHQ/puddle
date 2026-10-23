// SPDX-License-Identifier: GPL-3.0-or-later
import { render } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";
import DuckMark from "./DuckMark.svelte";

describe("DuckMark", () => {
  it("is decorative unless it is given a label", () => {
    const { container } = render(DuckMark);
    const svg = container.querySelector("svg");
    expect(svg?.getAttribute("aria-hidden")).toBe("true");
    expect(svg?.getAttribute("role")).toBeNull();
    expect(svg?.getAttribute("width")).toBe("24");
  });

  it("names itself as an image when it stands alone", () => {
    const { getByRole } = render(DuckMark, { label: "Puddle", size: 48 });
    const svg = getByRole("img", { name: "Puddle" });
    expect(svg.getAttribute("height")).toBe("48");
    expect(svg.getAttribute("aria-hidden")).toBeNull();
  });

  it("draws the water only when asked to", () => {
    expect(render(DuckMark).container.querySelector(".water")).not.toBeNull();
    expect(
      render(DuckMark, { water: false }).container.querySelector(".water"),
    ).toBeNull();
  });
});
