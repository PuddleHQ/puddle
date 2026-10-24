// SPDX-License-Identifier: GPL-3.0-or-later
import "@testing-library/jest-dom/vitest";

// The first render and the first role query in a test file build Svelte's and testing-library's
// tables and parse the stylesheet jsdom checks visibility against. That cost fell on whichever
// test ran first, where a loaded host turned it into a timeout; pay it here, before any test's
// clock starts.
{
  const { cleanup, fireEvent, render, screen } =
    await import("@testing-library/svelte");
  const { default: Warmup } = await import("./src/lib/testing/Warmup.svelte");
  render(Warmup);
  await fireEvent.click(screen.getByRole("button", { name: "More" }));
  screen.getByRole("heading", { name: "Warm-up" });
  screen.getAllByRole("listitem");
  screen.getByLabelText("Text");
  cleanup();
}
