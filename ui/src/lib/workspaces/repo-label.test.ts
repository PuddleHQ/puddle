// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { repoLabel } from "./repo-label.ts";

describe("repoLabel", () => {
  it("shortens a clone address to owner and repository", () => {
    expect(repoLabel("https://github.com/acme/web-shop.git")).toBe(
      "acme/web-shop",
    );
  });

  it("leaves an address it cannot read as typed", () => {
    expect(repoLabel("not a repository")).toBe("not a repository");
  });
});
