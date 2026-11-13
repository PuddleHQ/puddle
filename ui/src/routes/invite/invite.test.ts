// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, expect, it } from "vitest";
import Page from "./+page.svelte";

afterEach(cleanup);

it("tells a browser that is already signed in the invite link was used, and where to go", () => {
  render(Page);
  expect(
    screen.getByRole("heading", { name: "This invite link has been used" }),
  ).toBeVisible();
  expect(screen.getByRole("link", { name: "Puddle" })).toHaveAttribute(
    "href",
    "/",
  );
});
