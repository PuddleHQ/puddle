// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { expectNoIdsShown } from "./no-ids.ts";

function html(markup: string): HTMLElement {
  const root = document.createElement("div");
  root.innerHTML = markup;
  return root;
}

describe("the check that a generated id is not shown", () => {
  it("fails on the id in text, an accessible name, a title or a placeholder", () => {
    for (const markup of [
      "<p>Latest: g07jxx5q wants</p>",
      '<button aria-label="Delete g07jxx5q">x</button>',
      '<span title="g07jxx5q">Shop</span>',
      '<input placeholder="g07jxx5q" />',
    ]) {
      expect(
        () => expectNoIdsShown(html(markup), ["g07jxx5q"]),
        markup,
      ).toThrow();
    }
  });
  it("lets an id stay in a value or a link the program follows", () => {
    expectNoIdsShown(
      html(
        '<select><option value="g07jxx5q">Shop</option></select><a href="/workspaces/g07jxx5q">Shop</a>',
      ),
      ["g07jxx5q"],
    );
  });
});
