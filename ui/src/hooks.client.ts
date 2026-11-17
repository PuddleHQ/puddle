// SPDX-License-Identifier: AGPL-3.0-or-later
// An error no code expected (in a load, a render or an effect) shows the error page. SvelteKit
// says only "Internal Error" there; Details adds what was thrown, the console gets the error and
// the server's log gets a scrubbed report of it. So do errors that never reach the error page:
// one thrown in an event handler or a timer, and a promise nothing handled.
import type { ClientInit, HandleClientError } from "@sveltejs/kit/hooks";
import { version } from "$app/env";
import { api } from "#lib/api/client.ts";
import { describeError } from "#lib/errors/unexpected.ts";
import { ErrorReporter, reportUncaught } from "#lib/errors/report.ts";

export const reporter = new ErrorReporter({
  send: async (body) => {
    const { response } = await api.POST("/api/ui-errors", { body });
    const retry = Number(response.headers.get("retry-after"));
    return {
      status: response.status,
      ...(Number.isFinite(retry) && retry > 0 ? { retryAfterS: retry } : {}),
    };
  },
  route: () => window.location.pathname,
  build: version,
});

export const init: ClientInit = () => {
  reportUncaught(window, reporter);
};

export const handleError: HandleClientError = ({ error, kind }) => {
  console.error(error);
  // Only a bug: an `error(...)` the app threw on purpose, or a page that is not there, is not one.
  if (kind === "unknown") reporter.report("page", error);
  return { detail: describeError(error) };
};
