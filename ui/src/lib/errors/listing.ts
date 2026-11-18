// SPDX-License-Identifier: AGPL-3.0-or-later
// The errors the app's pages reported, as Settings lists them.
import type { components } from "#lib/api/schema.d.ts";
import { absoluteTime } from "#lib/format/relative-time.ts";

export type UiError = components["schemas"]["UiError"];

export const KIND_LABEL: Record<UiError["kind"], string> = {
  page: "Error page shown",
  uncaught: "Uncaught error",
  unhandled_rejection: "Unhandled promise",
};

/** One error as text to paste into a report: when, where, which build, what and the stack. */
export function errorText(error: UiError, locale?: string): string {
  return [
    `${KIND_LABEL[error.kind]} at ${absoluteTime(error.at, locale)}`,
    `Page: ${error.route ?? "unknown"}`,
    `Build: ${error.build ?? "unknown"}`,
    error.message,
    ...(error.stack === null ? [] : [error.stack]),
  ].join("\n");
}
