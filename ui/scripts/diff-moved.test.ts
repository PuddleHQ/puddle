// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { movedLines } from "./diff-moved.ts";

const header = (path: string) =>
  `diff --git a/${path} b/${path}\n--- a/${path}\n+++ b/${path}\n`;

const body = [
  "let first = compute_the_first_value();",
  "let second = compute_the_second_value()?;",
  "}",
];

describe("movedLines", () => {
  it("marks lines cut from one file and pasted in another", () => {
    const diff =
      header("a.rs") +
      "@@ -10,3 +9,0 @@\n" +
      body.map((l) => `-    ${l}`).join("\n") +
      "\n" +
      header("b.rs") +
      "@@ -0,0 +4,3 @@\n" +
      body.map((l) => `+${l}`).join("\n") +
      "\n";
    expect([...movedLines(diff)].sort()).toEqual([
      "b.rs:4",
      "b.rs:5",
      "b.rs:6",
    ]);
  });

  it("does not mark a copy, because the old place keeps the lines", () => {
    const diff =
      header("b.rs") +
      "@@ -0,0 +4,3 @@\n" +
      body.map((l) => `+${l}`).join("\n") +
      "\n";
    expect(movedLines(diff).size).toBe(0);
  });

  it("uses a removed line once, so two pastes of one cut mark only one", () => {
    const diff =
      header("a.rs") +
      "@@ -10,3 +9,0 @@\n" +
      body.map((l) => `-${l}`).join("\n") +
      "\n" +
      header("b.rs") +
      "@@ -0,0 +1,3 @@\n" +
      body.map((l) => `+${l}`).join("\n") +
      "\n@@ -0,0 +20,3 @@\n" +
      body.map((l) => `+${l}`).join("\n") +
      "\n";
    expect(movedLines(diff).size).toBe(3);
  });

  it("ignores a run under 20 letters and digits, such as braces and a short call", () => {
    const diff =
      header("a.rs") +
      "@@ -1,2 +0,0 @@\n-    x?;\n-}\n" +
      header("b.rs") +
      "@@ -0,0 +1,2 @@\n+x?;\n+}\n";
    expect(movedLines(diff).size).toBe(0);
  });

  it("does not join lines that were apart in the old file", () => {
    const diff =
      header("a.rs") +
      "@@ -1 +0,0 @@\n-let a = foo(1);\n@@ -9 +8,0 @@\n-let b = bar(2);\n" +
      header("b.rs") +
      "@@ -0,0 +1,2 @@\n+let a = foo(1);\n+let b = bar(2);\n";
    expect(movedLines(diff).size).toBe(0);
  });

  it("marks the unchanged line of a block and not the one that changed", () => {
    const diff =
      header("a.rs") +
      "@@ -1,3 +0,0 @@\n" +
      body.map((l) => `-${l}`).join("\n") +
      "\n" +
      header("b.rs") +
      "@@ -0,0 +1,3 @@\n+let first = compute_the_first_value();\n+let second = compute_the_other_value()?;\n+}\n";
    expect([...movedLines(diff)]).toEqual(["b.rs:1"]);
  });

  it("reads a removed line that starts with dashes as a line, not a file header", () => {
    const diff =
      header("a.rs") +
      "@@ -1,2 +0,0 @@\n--- a comment that is long enough to count\n-let first = compute_the_first_value();\n" +
      header("b.rs") +
      "@@ -0,0 +1,2 @@\n+-- a comment that is long enough to count\n+let first = compute_the_first_value();\n";
    expect(movedLines(diff).size).toBe(2);
  });

  it("marks a move inside one file", () => {
    const diff =
      header("a.rs") +
      "@@ -1,2 +0,0 @@\n-let first = compute_the_first_value();\n-let second = compute_the_second_value()?;\n@@ -30,0 +29,2 @@\n+let first = compute_the_first_value();\n+let second = compute_the_second_value()?;\n";
    expect([...movedLines(diff)].sort()).toEqual(["a.rs:29", "a.rs:30"]);
  });
});
