// SPDX-License-Identifier: GPL-3.0-or-later
// Why a read failed, in words a screen can show: the API's own message when it sent one, else
// what the status code means, else that the service did not answer.
import { sentence } from "#lib/rules/model.ts";

export const SERVICE_DOWN = "puddle's service isn't answering.";

/** A failed read: the API's error body (if any) and the HTTP response (if one arrived). */
export function readFailure(
  error: { message?: string } | undefined,
  response?: { status: number },
): string {
  if (error?.message) return sentence(error.message);
  if (!response) return SERVICE_DOWN;
  if (response.status === 401) return "puddle didn't accept this app's key.";
  if (response.status >= 500) {
    return `puddle's service hit an error (status ${response.status}).`;
  }
  return `puddle answered with status ${response.status}.`;
}
