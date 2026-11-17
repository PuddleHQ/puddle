// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import { handleError } from "./hooks.client.ts";

describe("handleError", () => {
  it("adds what was thrown as the error page's details and keeps SvelteKit's message", async () => {
    const logged = vi.spyOn(console, "error").mockImplementation(() => {});
    const error = new TypeError("read only");
    error.stack = "TypeError: read only\n    at x.js:1:2";
    const answer = await handleError({
      error,
      status: 500,
      message: "Internal Error",
      event: {} as never,
    } as never);
    expect(answer).toEqual({ detail: "TypeError: read only\n  at x.js:1:2" });
    expect(logged).toHaveBeenCalledWith(error);
    logged.mockRestore();
  });
});
