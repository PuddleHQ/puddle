// SPDX-License-Identifier: AGPL-3.0-or-later
// App-level notices: things that happened outside what the user is looking at (a workspace ran
// out of memory, stopped on its own, the network got worse). They stay in a list until dismissed
// or until the cause is gone. A notice with a key already in the list replaces it, so a repeat
// moves the old one instead of piling up.
//
// Showing a notice never moves focus; the list is a polite live region (Notices.svelte).

export type NoticeTone = "info" | "warning";

export interface NoticeLink {
  href: string;
  label: string;
}

/** A button on a notice: one deliberate click does the thing the notice says is missing. */
export interface NoticeAction {
  label: string;
  /** A rejection's message is shown to the user: in the confirmation when there is one. */
  run: () => void | Promise<void>;
  /**
   * Asks first, for a click that deletes something: the notice shows this dialog and calls
   * `run` only on its confirm button. The dialog stays open, and says why, when `run` fails.
   */
  confirm?: NoticeConfirm;
}

export interface NoticeConfirm {
  title: string;
  /** The one line that says what is removed. */
  summary: string;
  /** What happens next. */
  detail?: string;
  confirmLabel: string;
}

export interface NoticeInput {
  /** What the notice is about; a second notice with this key replaces the first. */
  key: string;
  tone: NoticeTone;
  /** One sentence. May quote names from a workspace: it is only ever drawn as text. */
  title: string;
  /** More, shown under the title. */
  detail?: string;
  link?: NoticeLink;
  action?: NoticeAction;
}

export interface Notice extends NoticeInput {
  id: number;
  /** Epoch ms it was raised. */
  at: number;
}

/** The most notices kept; a workspace chooses what raises one, so the list must not grow with it. */
const MAX_NOTICES = 200;
const MAX_TITLE = 300;
const MAX_DETAIL = 1000;
const MAX_LABEL = 120;

/** Control characters (but not a newline or tab), direction overrides and zero-width marks. */
const UNSAFE =
  /[\p{Cc}\u061C\u200B\u200C\u200E\u200F\u202A-\u202E\u2060\u2066-\u2069\uFEFF]/gu;

function scrub(text: string, max: number): string {
  const clean = text.replace(UNSAFE, (c) =>
    c === "\n" || c === "\t" ? c : "?",
  );
  const chars = Array.from(clean);
  return chars.length > max ? `${chars.slice(0, max).join("")}…` : clean;
}

/**
 * Names and hosts in a notice come from a workspace: a direction override could make "Allow push
 * for <repo>" read as something else. Every notice passes through here, so no raiser can forget.
 */
function safe(input: NoticeInput): NoticeInput {
  const out: NoticeInput = { ...input, title: scrub(input.title, MAX_TITLE) };
  if (input.detail !== undefined) out.detail = scrub(input.detail, MAX_DETAIL);
  if (input.action) {
    out.action = {
      ...input.action,
      label: scrub(input.action.label, MAX_LABEL),
    };
  }
  return out;
}

/** Past the limit the oldest info notices go first, then the oldest of any kind. */
function capped(items: Notice[]): Notice[] {
  let extra = items.length - MAX_NOTICES;
  if (extra <= 0) return items;
  const dropInfo = new Set<number>();
  for (const n of items) {
    if (extra === 0) break;
    if (n.tone === "info") {
      dropInfo.add(n.id);
      extra -= 1;
    }
  }
  const rest = items.filter((n) => !dropInfo.has(n.id));
  return extra > 0 ? rest.slice(extra) : rest;
}

export class NoticeCenter {
  items = $state.raw<Notice[]>([]);
  #next = 1;
  readonly #now: () => number;

  constructor(now: () => number = Date.now) {
    this.#now = now;
  }

  /** Newest last. Returns the notice's id. */
  add(input: NoticeInput): number {
    const id = this.#next++;
    const notice: Notice = { ...safe(input), id, at: this.#now() };
    this.items = capped([
      ...this.items.filter((n) => n.key !== input.key),
      notice,
    ]);
    return id;
  }

  dismiss(id: number): void {
    this.items = this.items.filter((n) => n.id !== id);
  }

  /** Removes the notice of a key when its cause has gone. */
  resolve(key: string): void {
    if (this.items.some((n) => n.key === key)) {
      this.items = this.items.filter((n) => n.key !== key);
    }
  }

  clear(): void {
    this.items = [];
  }
}

export const notices = new NoticeCenter();
