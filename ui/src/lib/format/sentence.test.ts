// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { sentence } from "./sentence.ts";

describe("sentence", () => {
  it("shows the API's lower-case messages as sentences", () => {
    expect(sentence("invalid pattern")).toBe("Invalid pattern.");
    expect(sentence("Already.")).toBe("Already.");
    expect(sentence("  ")).toBe("");
  });
});
