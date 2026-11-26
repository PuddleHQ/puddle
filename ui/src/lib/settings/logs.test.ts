// SPDX-License-Identifier: AGPL-3.0-or-later
// "Export log bundle" against a stand-in service that sees the raw request, and a browser whose
// download is a clicked link.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const wire = vi.hoisted(() => ({
  handler: (_request: Request): Promise<Response> =>
    Promise.reject(new Error("no handler")),
}));
vi.mock("#lib/api/client.ts", async () => {
  const real =
    await vi.importActual<typeof import("#lib/api/client.ts")>(
      "#lib/api/client.ts",
    );
  return {
    api: real.createApiClient({
      baseUrl: "http://127.0.0.1:1",
      getToken: () => "tok",
      fetch: (input) => wire.handler(input as Request),
    }),
  };
});

import { exportLogBundle } from "./logs.ts";

const saved: string[] = [];

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout"] });
  saved.length = 0;
  vi.stubGlobal("URL", {
    createObjectURL: () => "blob:fake",
    revokeObjectURL: () => undefined,
  });
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
    this: HTMLAnchorElement,
  ) {
    saved.push(this.download);
  });
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("exporting the log bundle", () => {
  it("posts, and saves the file under the name the service gave", async () => {
    let method = "";
    wire.handler = async (request) => {
      method = `${request.method} ${request.url}`;
      return new Response("bytes", {
        status: 200,
        headers: {
          "content-type": "application/gzip",
          "content-disposition": 'attachment; filename="puddle-logs-1.tar.gz"',
        },
      });
    };
    const result = await exportLogBundle();
    expect(method).toBe("POST http://127.0.0.1:1/api/logs/bundle");
    expect(result).toEqual({
      ok: true,
      name: "puddle-logs-1.tar.gz",
      bytes: 5,
    });
    expect(saved).toEqual(["puddle-logs-1.tar.gz"]);
  });

  it("says the service is not answering when the request fails", async () => {
    wire.handler = () => Promise.reject(new TypeError("down"));
    expect(await exportLogBundle()).toEqual({
      ok: false,
      reason: "Puddle's service isn't answering.",
    });
    expect(saved).toEqual([]);
  });
});
