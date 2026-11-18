// SPDX-License-Identifier: AGPL-3.0-or-later
// An error no code expected (a bug), as the error page's Details show it: what was thrown and where,
// so a user can copy it into a report.

/** The most stack lines shown; the first few name the place, the rest are the framework. */
export const STACK_LINES = 12;
/** The most characters shown, so a huge message can't flood the page. */
export const DETAIL_LIMIT = 4000;

/** What `error` was, in text: its name and message, then the top of its stack. */
export function describeError(error: unknown): string {
  let text: string;
  if (error instanceof Error) {
    const head = `${error.name}: ${error.message}`;
    const frames = (error.stack ?? "")
      .split("\n")
      .filter((line) => /^\s+at |@/.test(line))
      .slice(0, STACK_LINES)
      .map((line) => `  ${line.trim()}`);
    text = [head, ...frames].join("\n");
  } else {
    text = `Thrown: ${String(error)}`;
  }
  return text.length > DETAIL_LIMIT
    ? `${text.slice(0, DETAIL_LIMIT)}...`
    : text;
}
