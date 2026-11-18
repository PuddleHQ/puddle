// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "#lib/api/client.ts";
import {
  handleError,
  init,
  pagePath,
  reporter,
  sendReport,
} from "./hooks.client.ts";

afterEach(() => {
  vi.restoreAllMocks();
});

function caught() {
  const error = new TypeError("read only");
  error.stack = "TypeError: read only\n    at x.js:1:2";
  return { kind: "unknown", error, event: {} as never } as never;
}

function notFound() {
  return {
    kind: "framework",
    error: { status: 404, message: "Not Found" },
    event: {} as never,
  } as never;
}

describe("handleError", () => {
  it("adds what was thrown as the error page's details and keeps SvelteKit's message", async () => {
    const logged = vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(reporter, "report").mockImplementation(() => {});
    const input = caught();
    const answer = await handleError(input);
    expect(answer).toEqual({ detail: "TypeError: read only\n  at x.js:1:2" });
    expect(logged).toHaveBeenCalledWith((input as { error: unknown }).error);
  });

  it("reports a bug to the server, but not a page that is not there", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const report = vi.spyOn(reporter, "report").mockImplementation(() => {});
    const input = caught();
    await handleError(input);
    expect(report).toHaveBeenCalledExactlyOnceWith(
      "page",
      (input as { error: unknown }).error,
    );
    await handleError(notFound());
    expect(report).toHaveBeenCalledOnce();
  });
});

describe("init", () => {
  it("reports what the window sees that nothing caught", async () => {
    const report = vi.spyOn(reporter, "report").mockImplementation(() => {});
    await init();
    const error = new Error("in a click handler");
    window.dispatchEvent(
      new ErrorEvent("error", { error, message: error.message }),
    );
    expect(report).toHaveBeenCalledWith("uncaught", error);
  });
});

describe("sendReport", () => {
  const body = {
    kind: "page" as const,
    message: "Error: x",
    stack: null,
    route: "/",
    build: "b",
  };

  it("posts the report and reads the server's Retry-After", async () => {
    const POST = vi.fn(async () => ({
      response: new Response(null, {
        status: 429,
        headers: { "retry-after": "30" },
      }),
    }));
    expect(await sendReport({ POST } as never, body)).toEqual({
      status: 429,
      retryAfterS: 30,
    });
    expect(POST).toHaveBeenCalledWith("/api/ui-errors", { body });
  });

  it("leaves the wait out when the server names none", async () => {
    const POST = vi.fn(async () => ({
      response: new Response(null, { status: 204 }),
    }));
    expect(await sendReport({ POST } as never, body)).toEqual({ status: 204 });
  });
});

describe("reporter", () => {
  it("sends through the app's API client", async () => {
    const POST = vi.spyOn(api, "POST").mockResolvedValue({
      response: new Response(null, { status: 204 }),
    } as never);
    reporter.report("uncaught", new Error("through the client"));
    await vi.waitFor(() => expect(POST).toHaveBeenCalledOnce());
    expect(POST.mock.calls[0]?.[0]).toBe("/api/ui-errors");
  });
});

describe("pagePath", () => {
  it("is the page's path alone", () => {
    window.history.replaceState(
      null,
      "",
      "/workspaces/abc/terminal?ticket=t#x",
    );
    expect(pagePath()).toBe("/workspaces/abc/terminal");
  });
});
