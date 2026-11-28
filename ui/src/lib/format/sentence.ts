// SPDX-License-Identifier: AGPL-3.0-or-later

/** The API's messages are lower case with no full stop; the UI shows them as sentences. */
export function sentence(message: string): string {
  const trimmed = message.trim();
  if (trimmed === "") return trimmed;
  const first = trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
  return /[.!?]$/.test(first) ? first : `${first}.`;
}
