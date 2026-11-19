// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFileSync, readdirSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { describe, expect, it } from "vitest";

// The look of every form control comes from controls.css. A screen that adds a control the shared
// styles do not reach shows the browser's raw grey one (in Dark and in every named theme). This
// test reads each component and fails on such a control, naming the file and line.
const SRC = join(import.meta.dirname, "..", "..");
const css = readFileSync(join(import.meta.dirname, "controls.css"), "utf8");

/** A path with forward slashes whatever the platform separator is. */
function posix(path: string, separator: string): string {
  return path.split(separator).join("/");
}

function svelteFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return svelteFiles(path);
    return entry.name.endsWith(".svelte") ? [path] : [];
  });
}

const files = svelteFiles(SRC).map((path) => ({
  name: posix(relative(SRC, path), sep),
  text: readFileSync(path, "utf8"),
}));

/** The `type` of an input that controls.css styles, by the selector that styles it. */
const STYLED_TYPES = ["checkbox", "radio", "range", "color"];
/** Text-like types are styled by the one `:where(input:not(...))` rule, which excludes these. */
const NOT_TEXT = [
  "checkbox",
  "radio",
  "range",
  "color",
  "file",
  "hidden",
  "submit",
  "button",
  "reset",
  "image",
];

/** The text of a component's `<style>` block, or "" without one. */
function styleOf(text: string): string {
  return /<style[^>]*>([\s\S]*?)<\/style>/.exec(text)?.[1] ?? "";
}

/** A file input the user never sees: the `hidden` attribute, or a class the component's own style
 * sets to `display: none` (a button opens the picker). */
function isHiddenInput(attrs: string, style: string): boolean {
  if (/\bhidden\b/.test(attrs)) return true;
  const classes = /class=(?:"([^"]*)"|\{([^}]*)\})/.exec(attrs) ?? [];
  const names = (classes[1] ?? classes[2] ?? "")
    .split(/[\s"'`${}?:()]+/)
    .filter(Boolean);
  return names.some((c) =>
    new RegExp(
      `\\.${c}\\b[^{]*\\{[^}]*\\bdisplay\\s*:\\s*none\\b[^}]*\\}`,
      "s",
    ).test(style),
  );
}

function lineOf(text: string, index: number): number {
  return text.slice(0, index).split("\n").length;
}

/** Every `<tag ...>` opening of one of the form controls. */
function openings(text: string, tag: string) {
  return [...text.matchAll(new RegExp(`<${tag}\\b([^>]*?)/?>`, "gs"))].map(
    (m) => ({ attrs: m[1] ?? "", line: lineOf(text, m.index) }),
  );
}

describe("the shared control styles", () => {
  it("style the text rule, the select and the textarea with the control tokens", () => {
    const rule =
      /:where\(\s*input:not\([^)]*\),\s*select,\s*textarea\s*\)\s*\{([^}]*)\}/s.exec(
        css,
      );
    expect(rule, "the :where() rule for text controls").not.toBeNull();
    const body = rule?.[1] ?? "";
    for (const token of [
      "--control-size",
      "--color-border",
      "--color-bg",
      "--color-text",
    ]) {
      expect(body, token).toContain(token);
    }
  });

  it("name every non-text input type that a screen uses", () => {
    for (const type of STYLED_TYPES) {
      expect(css, type).toContain(`input[type="${type}"]`);
    }
  });
});

describe("every form control in the product", () => {
  it("reaches the shared styles", () => {
    const unstyled: string[] = [];
    for (const { name, text } of files) {
      for (const { attrs, line } of openings(text, "input")) {
        const type = /\btype=["'{]*([a-z]+)/.exec(attrs)?.[1] ?? "text";
        if (type === "file") {
          if (!isHiddenInput(attrs, styleOf(text)))
            unstyled.push(`${name}:${line} a visible file input`);
        } else if (NOT_TEXT.includes(type) && !STYLED_TYPES.includes(type)) {
          if (type !== "hidden" && type !== "button" && type !== "submit")
            unstyled.push(`${name}:${line} input type=${type}`);
        }
      }
    }
    expect(unstyled).toEqual([]);
  });

  it("gives every button the shared look or a look of its own", () => {
    const unstyled: string[] = [];
    for (const { name, text } of files) {
      const style = styleOf(text);
      for (const { attrs, line } of openings(text, "button")) {
        const classes =
          /class(?::\w+)?=(?:"([^"]*)"|\{([^}]*)\})/.exec(attrs) ?? [];
        const names = (classes[1] ?? classes[2] ?? "")
          .split(/[\s"'`${}?:()]+/)
          .filter(Boolean);
        if (names.some((c) => c === "btn" || c.startsWith("btn-"))) continue;
        const own = names.some((c) =>
          new RegExp(
            `\\.${c}\\b[^{]*\\{[^}]*\\bbackground\\b[^}]*\\}`,
            "s",
          ).test(style),
        );
        if (!own)
          unstyled.push(`${name}:${line} button class="${names.join(" ")}"`);
      }
    }
    // The warm-up page renders no product UI.
    expect(unstyled.filter((u) => !u.startsWith("lib/testing/"))).toEqual([]);
  });

  it("is a gate that fires: a bare button and a visible file input are found", () => {
    const text = `<button type="button">x</button>\n<input type="file" />`;
    expect(openings(text, "button")).toHaveLength(1);
    expect(openings(text, "input")[0]?.attrs).not.toMatch(/\bhidden\b/);
    expect(isHiddenInput(openings(text, "input")[0]?.attrs ?? "", "")).toBe(
      false,
    );
  });

  it("accepts a file input hidden by its component's own style, and no other", () => {
    const style = ".file { display: none; }\n.other { color: red; }";
    expect(isHiddenInput('class="file" type="file"', style)).toBe(true);
    expect(isHiddenInput('type="file" hidden', "")).toBe(true);
    expect(isHiddenInput('class="other" type="file"', style)).toBe(false);
    expect(
      isHiddenInput('class="file" type="file"', ".file { color: red; }"),
    ).toBe(false);
  });

  it("names every file with forward slashes, so a path filter matches on Windows too", () => {
    expect(posix("lib\\testing\\Warmup.svelte", "\\")).toBe(
      "lib/testing/Warmup.svelte",
    );
    expect(posix("lib/testing/Warmup.svelte", "/")).toBe(
      "lib/testing/Warmup.svelte",
    );
    const warmup = files.find((f) => f.name.startsWith("lib/testing/"));
    expect(
      warmup,
      "the warm-up page is found by its forward-slash path",
    ).toBeDefined();
  });
});
