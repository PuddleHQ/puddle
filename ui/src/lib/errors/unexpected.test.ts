// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { DETAIL_LIMIT, describeError, STACK_LINES } from "./unexpected.ts";

describe("describeError", () => {
  it("names the error, its message and where it was thrown", () => {
    const error = new TypeError(
      "Cannot assign to read only property 'toString' of object '#<Object>'",
    );
    error.stack = [
      `TypeError: ${error.message}`,
      "    at http://127.0.0.1:4711/_app/immutable/nodes/31.js:9:7650",
      "    at http://127.0.0.1:4711/_app/immutable/nodes/31.js:9:8105",
    ].join("\n");
    expect(describeError(error)).toBe(
      [
        "TypeError: Cannot assign to read only property 'toString' of object '#<Object>'",
        "  at http://127.0.0.1:4711/_app/immutable/nodes/31.js:9:7650",
        "  at http://127.0.0.1:4711/_app/immutable/nodes/31.js:9:8105",
      ].join("\n"),
    );
  });

  it("reads WebKit's stack form too", () => {
    const error = new Error("boom");
    error.stack = "open@http://127.0.0.1:4711/_app/x.js:1:2\n";
    expect(describeError(error)).toBe(
      "Error: boom\n  open@http://127.0.0.1:4711/_app/x.js:1:2",
    );
  });

  it("keeps only the top of a long stack", () => {
    const error = new Error("deep");
    error.stack = [
      "Error: deep",
      ...Array.from({ length: 40 }, (_, i) => `    at f${i} (x.js:${i}:1)`),
    ].join("\n");
    const lines = describeError(error).split("\n");
    expect(lines).toHaveLength(1 + STACK_LINES);
    expect(lines.at(-1)).toBe(
      `  at f${STACK_LINES - 1} (x.js:${STACK_LINES - 1}:1)`,
    );
  });

  it("says what was thrown when it is not an Error", () => {
    expect(describeError("plain text")).toBe("Thrown: plain text");
    expect(describeError(undefined)).toBe("Thrown: undefined");
  });

  it("cuts a huge message", () => {
    const text = describeError(new Error("x".repeat(DETAIL_LIMIT * 2)));
    expect(text).toHaveLength(DETAIL_LIMIT + 3);
    expect(text.endsWith("...")).toBe(true);
  });
});
