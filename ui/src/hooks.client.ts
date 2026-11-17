// SPDX-License-Identifier: AGPL-3.0-or-later
// An error no code expected (in a load, a render or an effect) shows the error page. SvelteKit
// says only "Internal Error" there; Details adds what was thrown.
import type { HandleClientError } from "@sveltejs/kit/hooks";
import { describeError } from "#lib/errors/unexpected.ts";

export const handleError: HandleClientError = ({ error }) => ({
  detail: describeError(error),
});
