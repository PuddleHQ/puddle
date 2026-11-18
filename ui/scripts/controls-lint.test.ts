// SPDX-License-Identifier: AGPL-3.0-or-later
import { resolve } from "node:path";
import { ESLint } from "eslint";
import svelteParser from "svelte-eslint-parser";
import { describe, expect, it } from "vitest";
import { bareControlSelectors, noBareControls } from "../eslint.controls.js";

const eslint = new ESLint({
  cwd: resolve(import.meta.dirname, ".."),
  overrideConfigFile: true,
  overrideConfig: [
    { files: ["**/*.svelte"], languageOptions: { parser: svelteParser } },
    noBareControls,
  ],
});

// What `npm run lint` says about a component with this style block.
async function lint(style: string) {
  const [result] = await eslint.lintText(
    `<input id="x" />\n<style>\n${style}\n</style>\n`,
    { filePath: resolve(import.meta.dirname, "../src/probe.svelte") },
  );
  return (result?.messages ?? []).filter(
    (m) => m.ruleId === "puddle/no-bare-controls",
  );
}

await lint("");

describe("the rule against a component that styles a bare control", () => {
  it("fails the old per-file copies, in every spelling they took", async () => {
    for (const bad of [
      `input { border: 1px solid red; }`,
      `.field input, .field select { color: red; }`,
      `input[type="text"] { color: red; }`,
      `textarea[aria-invalid="true"] { color: red; }`,
      `:global(.form-dialog select) { color: red; }`,
      `.menu button:hover:not(:disabled) { color: red; }`,
      `input.host { width: 7rem; }`,
    ]) {
      const found = await lint(bad);
      expect(found.length, bad).toBeGreaterThan(0);
      expect(found[0]?.severity).toBe(2);
      expect(found[0]?.message).toContain("controls.css");
    }
  });

  it("reports the line of the selector inside the file", async () => {
    const found = await lint(`.a { color: red; }\n\nselect { color: red; }`);
    expect(found.map((m) => m.line)).toEqual([5]);
  });

  it("passes a class for layout, a comment and a media query", async () => {
    expect(
      await lint(
        `.input-fill { flex: 1; }\n/* input { color: red; } */\n@media (width < 40rem) { .row { gap: 0; } }\n.input-short:focus { color: red; }`,
      ),
    ).toEqual([]);
  });
});

describe("bareControlSelectors", () => {
  it("names each offending selector once", () => {
    expect(
      bareControlSelectors("input, .a select { x: y }").map((f) => f.selector),
    ).toEqual(["input", ".a select"]);
  });
});
