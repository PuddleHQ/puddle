// SPDX-License-Identifier: AGPL-3.0-or-later
// The editor defaults (colour theme, font size, extensions, key bindings) as text fields: what a
// stored value looks like in a box, and what typed text becomes. The rules are the API's own
// (crates/puddle-settings/src/editor.rs), checked here first so a refusal never has to come back
// from the service. An empty box at the global level clears the field; in a workspace it clears
// only for the fields where "nothing" is a value of its own (see `EMPTY_MEANS`).
import type { components } from "#lib/api/schema.d.ts";

export type EditorSettings = components["schemas"]["EditorSettings"];
export type EditorField = keyof EditorSettings;
export type Parsed<T> = { ok: true; value: T } | { ok: false; message: string };

export const THEME_MAX_CHARS = 100;
export const FONT_MIN = 8;
export const FONT_MAX = 40;
export const EXTENSIONS_MAX = 50;
export const KEYBINDINGS_MAX_BYTES = 64 * 1024;

const ok = <T>(value: T): Parsed<T> => ({ ok: true, value });
const bad = (message: string): Parsed<never> => ({ ok: false, message });

/** A layer that sets nothing. */
export const NO_EDITOR: EditorSettings = {
  colour_theme: null,
  font_size: null,
  extensions: null,
  keybindings: null,
};

/** The layer as stored, tolerating a document from before the group existed. */
export const editorOf = (
  layer: { editor?: EditorSettings | undefined } | null | undefined,
): EditorSettings => ({ ...NO_EDITOR, ...layer?.editor });

export function parseTheme(text: string): Parsed<string | null> {
  const name = text.trim();
  if (name === "") return ok(null);
  if ([...name].length > THEME_MAX_CHARS)
    return bad(`Use at most ${THEME_MAX_CHARS} characters.`);
  // eslint-disable-next-line no-control-regex -- the API refuses control characters, so this says so first
  if (/[\u0000-\u001f\u007f]/.test(name))
    return bad("A theme name has no control characters.");
  return ok(name);
}

export function parseFontSize(text: string): Parsed<number | null> {
  const t = text.trim();
  if (t === "") return ok(null);
  const n = /^\d{1,3}$/.test(t) ? Number(t) : NaN;
  if (!(n >= FONT_MIN && n <= FONT_MAX))
    return bad(`Use a whole number from ${FONT_MIN} to ${FONT_MAX}.`);
  return ok(n);
}

const ID_WORD = /^[A-Za-z0-9][A-Za-z0-9_-]*$/;
const ID_NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;

/** One extension id in `publisher.name` form, lower-cased as the API stores it. */
export function parseExtensionId(id: string): Parsed<string> {
  const dot = id.indexOf(".");
  const publisher = dot < 0 ? "" : id.slice(0, dot);
  const name = dot < 0 ? "" : id.slice(dot + 1);
  if (id.length > 200 || !ID_WORD.test(publisher) || !ID_NAME.test(name))
    return bad(`"${id}" is not an extension id like publisher.name.`);
  return ok(id.toLowerCase());
}

/**
 * Ids separated by commas, spaces or new lines: lower-cased, sorted and without repeats, as the API
 * keeps them. Empty text is an empty list; the caller decides whether that means "none" or "unset".
 */
export function parseExtensions(text: string): Parsed<string[]> {
  const out = new Set<string>();
  for (const raw of text.split(/[\s,]+/).filter(Boolean)) {
    const id = parseExtensionId(raw);
    if (!id.ok) return id;
    out.add(id.value);
  }
  if (out.size > EXTENSIONS_MAX)
    return bad(`List at most ${EXTENSIONS_MAX} extensions.`);
  return ok([...out].sort());
}

export const extensionsText = (list: readonly string[] | null): string =>
  (list ?? []).join("\n");

/** Key bindings: the text of a JSON array of `{key, command}` objects. Empty text is no bindings (`null`). */
export function parseKeybindings(text: string): Parsed<string | null> {
  const t = text.trim();
  if (t === "") return ok(null);
  if (new TextEncoder().encode(t).length > KEYBINDINGS_MAX_BYTES)
    return bad("Use at most 64 KiB.");
  let json: unknown;
  try {
    json = JSON.parse(t);
  } catch {
    return bad("This is not valid JSON.");
  }
  if (!Array.isArray(json))
    return bad('Use a JSON array, like [{"key": "ctrl+k", "command": "..."}].');
  const text1 = (v: unknown) => typeof v === "string" && v.trim() !== "";
  if (
    !json.every(
      (e: unknown) =>
        typeof e === "object" &&
        e !== null &&
        text1((e as Record<string, unknown>)["key"]) &&
        text1((e as Record<string, unknown>)["command"]),
    )
  )
    return bad('Every entry needs a text "key" and a text "command".');
  return ok(t);
}

/** What a workspace's empty box means for a field: "none" is a value there, the rest inherit. */
export const EMPTY_MEANS = {
  colour_theme: null,
  font_size: null,
  extensions: [] as string[],
  keybindings: "[]",
} as const;

/** The font size in a box, and how the layers word what they hold. */
export const fontSizeText = (px: number | null): string =>
  px === null ? "" : String(px);

/** What the effective values say in words, for a hint when the box is empty. */
export const THEME_DEFAULT = "Follows the system colour scheme";
export const FONT_DEFAULT = "The editor's own size";
export const EXTENSIONS_DEFAULT = "None";
export const KEYBINDINGS_DEFAULT = "None added";
