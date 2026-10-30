// SPDX-License-Identifier: GPL-3.0-or-later
// Every character the UI's own source puts on screen must be in a bundled font. A character the
// fonts lack is drawn from a system font, and the first such character makes the browser search
// the system's fonts; on Windows that search waits on the system font cache and held the page for
// seconds. Text that comes from the API or the user (a repository name, a log line) is not
// covered: only the source is read. JavaScript escapes such as "✓" are not decoded either.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const fontsCss = readFileSync(join(import.meta.dirname, "fonts.css"), "utf8");

/** The code points of the `unicode-range`s of the bundled sans-serif font. */
function interRanges(): Array<[number, number]> {
  const faces = fontsCss
    .split("@font-face")
    .filter((face) => face.includes('"Inter Variable"'));
  return faces.flatMap((face) => {
    const list = /unicode-range:([^;]+);/.exec(face)?.[1] ?? "";
    return list.split(",").map((part): [number, number] => {
      const [from, to = from] = part.trim().replace(/^u\+/i, "").split("-");
      return [parseInt(from ?? "", 16), parseInt(to ?? "", 16)];
    });
  });
}

const RANGES = interRanges();

/** `text` with `pattern` replaced until none is left: removing one match can join the halves of another. */
function stripAll(text: string, pattern: RegExp, replacement: string): string {
  let current = text;
  for (let next = current.replace(pattern, replacement); next !== current;) {
    current = next;
    next = current.replace(pattern, replacement);
  }
  return current;
}

/** The text a source file can show: comments removed, HTML entities decoded. */
function shownText(source: string): string {
  const code = stripAll(
    stripAll(stripAll(source, /<!--[\s\S]*?-->/g, ""), /\/\*[\s\S]*?\*\//g, ""),
    /(^|[^:])\/\/.*$/gm,
    "$1",
  );
  const decoder = document.createElement("textarea");
  return code.replace(/&#?\w+;/g, (entity) => {
    decoder.innerHTML = entity;
    return decoder.value;
  });
}

/** The characters of `text` that no bundled font has. */
function uncovered(text: string): string[] {
  return [...new Set(text)].filter((char) => {
    const point = char.codePointAt(0) ?? 0;
    return !RANGES.some(([from, to]) => point >= from && point <= to);
  });
}

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    const shown =
      /\.(svelte|ts)$/.test(entry.name) && !/\.(test|d)\.ts$/.test(entry.name);
    return shown ? [path] : [];
  });
}

describe("the bundled fonts", () => {
  it("declare Latin and the punctuation the UI uses", () => {
    expect(uncovered("Checking this computer… “Puddle” – ok")).toEqual([]);
  });

  it("lack the symbols that need a system font", () => {
    expect(uncovered(shownText("&larr; Settings ✓ ✕"))).toEqual([
      "←",
      "✓",
      "✕",
    ]);
  });

  it("have every character the UI's source shows", () => {
    const src = join(import.meta.dirname, "..", "..");
    const found = sourceFiles(src).flatMap((file) =>
      uncovered(shownText(readFileSync(file, "utf8"))).map(
        (char) => `${file.slice(src.length + 1)}: ${char}`,
      ),
    );
    expect(found).toEqual([]);
  });
});
