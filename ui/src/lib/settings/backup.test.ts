// SPDX-License-Identifier: AGPL-3.0-or-later
// "Back up now" against a stand-in service that sees the raw request, and a browser whose download
// is a clicked link.
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

import { backUpNow, fileNameFrom, sizeText } from "./backup.ts";

const saved: { name: string; href: string }[] = [];
const revoked: string[] = [];

beforeEach(() => {
  saved.length = 0;
  revoked.length = 0;
  wire.handler = () => Promise.reject(new TypeError("down"));
  vi.stubGlobal("URL", {
    createObjectURL: () => "blob:fake",
    revokeObjectURL: (url: string) => revoked.push(url),
  });
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
    this: HTMLAnchorElement,
  ) {
    saved.push({ name: this.download, href: this.href });
  });
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("backing up", () => {
  it("posts with the key, saves the file under the name the service gave and cleans up", async () => {
    let seen: { method: string; auth: string | null } | null = null;
    wire.handler = async (request) => {
      seen = {
        method: request.method,
        auth: request.headers.get("Authorization"),
      };
      return new Response("fake bytes", {
        status: 200,
        headers: {
          "Content-Type": "application/gzip",
          "Content-Disposition":
            'attachment; filename="puddle-backup-20261115T225500Z.tar.gz"',
        },
      });
    };
    const result = await backUpNow();
    expect(result).toEqual({
      ok: true,
      name: "puddle-backup-20261115T225500Z.tar.gz",
      bytes: 10,
    });
    expect(seen).toEqual({ method: "POST", auth: "Bearer tok" });
    expect(saved).toEqual([
      { name: "puddle-backup-20261115T225500Z.tar.gz", href: "blob:fake" },
    ]);
    expect(revoked).toEqual(["blob:fake"]);
    expect(document.querySelector("a[download]")).toBeNull();
  });

  it("says why when the service refuses, and saves nothing", async () => {
    wire.handler = async () =>
      new Response(
        JSON.stringify({
          error: "unavailable",
          message: "no database at /data/puddle.db",
        }),
        { status: 503, headers: { "Content-Type": "application/json" } },
      );
    const result = await backUpNow();
    expect(result).toEqual({
      ok: false,
      reason: "No database at /data/puddle.db.",
    });
    expect(saved).toEqual([]);
  });

  it("falls back to the status when the refusal has no readable body", async () => {
    wire.handler = async () => new Response("<html>", { status: 502 });
    const result = await backUpNow();
    expect(result).toEqual({
      ok: false,
      reason: "Puddle's service hit an error (status 502).",
    });
  });

  it("says the service is not answering when it cannot be reached", async () => {
    expect(await backUpNow()).toEqual({
      ok: false,
      reason: "Puddle's service isn't answering.",
    });
  });
});

describe("the file name and size", () => {
  it("reads the name from the header and never takes a path", () => {
    expect(fileNameFrom('attachment; filename="a.tar.gz"')).toBe("a.tar.gz");
    expect(fileNameFrom('attachment; filename="../x"')).toBe(
      "puddle-backup.tar.gz",
    );
    expect(fileNameFrom(null)).toBe("puddle-backup.tar.gz");
  });

  it("writes sizes the way a person reads them", () => {
    expect(sizeText(40)).toBe("40 bytes");
    expect(sizeText(312 * 1024)).toBe("312 KB");
    expect(sizeText(1.5 * 1024 * 1024)).toBe("1.5 MB");
  });
});
