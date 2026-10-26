// SPDX-License-Identifier: GPL-3.0-or-later
// The three icon calls against a stand-in service that sees the raw requests.
import { beforeEach, describe, expect, it, vi } from "vitest";

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

import {
  isIconChoice,
  loadIcons,
  MAX_ICON_BYTES,
  pngUrl,
  removeIcon,
  uploadIcon,
} from "./app-icon.ts";

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });

beforeEach(() => {
  wire.handler = () => Promise.reject(new TypeError("down"));
});

describe("uploading", () => {
  it("sends the file's own bytes as they are, with the key and a binary type", async () => {
    let seen: {
      type: string | null;
      auth: string | null;
      bytes: number[];
    } | null = null;
    wire.handler = async (request) => {
      seen = {
        type: request.headers.get("content-type"),
        auth: request.headers.get("authorization"),
        bytes: [...new Uint8Array(await request.arrayBuffer())],
      };
      expect(request.method).toBe("PUT");
      expect(new URL(request.url).pathname).toBe("/api/app-icons/custom");
      return json(200, { png_base64: "AAAA" });
    };
    const file = new File([new Uint8Array([137, 80, 78, 71, 0, 255])], "a.png");
    expect(await uploadIcon(file)).toEqual({ ok: true, value: "AAAA" });
    expect(seen).toEqual({
      type: "application/octet-stream",
      auth: "Bearer tok",
      bytes: [137, 80, 78, 71, 0, 255],
    });
  });

  it("gives the service's reason when the image is refused", async () => {
    wire.handler = async () =>
      json(422, {
        error: "invalid",
        message: "the image is completely transparent",
      });
    expect(await uploadIcon(new File([new Uint8Array(3)], "a.png"))).toEqual({
      ok: false,
      reason: "The image is completely transparent.",
    });
  });

  it("does not send a file over the limit", async () => {
    const big = new File([new Uint8Array(1)], "big.png");
    Object.defineProperty(big, "size", { value: MAX_ICON_BYTES + 1 });
    const result = await uploadIcon(big);
    expect(result).toEqual({
      ok: false,
      reason: "That file is larger than 8 MB.",
    });
  });

  it("says the service did not answer", async () => {
    expect(await uploadIcon(new File([new Uint8Array(3)], "a.png"))).toEqual({
      ok: false,
      reason: "Puddle's service isn't answering.",
    });
  });
});

describe("reading and removing", () => {
  it("reads the icons, or why not", async () => {
    wire.handler = async () =>
      json(200, { built_in: [], custom_png_base64: null });
    expect(await loadIcons()).toEqual({
      ok: true,
      value: { built_in: [], custom_png_base64: null },
    });
    wire.handler = async () =>
      json(500, { error: "internal", message: "internal error" });
    expect(await loadIcons()).toEqual({ ok: false, reason: "Internal error." });
    wire.handler = () => Promise.reject(new TypeError("down"));
    expect(await loadIcons()).toEqual({
      ok: false,
      reason: "Puddle's service isn't answering.",
    });
  });

  it("removes the image, or says why it cannot", async () => {
    wire.handler = async (request) => {
      expect(request.method).toBe("DELETE");
      return new Response(null, { status: 204 });
    };
    expect(await removeIcon()).toEqual({ ok: true, value: null });
    wire.handler = async () =>
      json(409, {
        error: "conflict",
        message: "your own image is the icon in use",
      });
    expect(await removeIcon()).toEqual({
      ok: false,
      reason: "Your own image is the icon in use.",
    });
    wire.handler = () => Promise.reject(new TypeError("down"));
    expect((await removeIcon()).ok).toBe(false);
  });
});

describe("small helpers", () => {
  it("knows the five choices and makes a data address for a PNG", () => {
    for (const v of ["duck", "duck_pond", "duck_night", "duck_sun", "custom"]) {
      expect(isIconChoice(v)).toBe(true);
    }
    expect(isIconChoice("penguin")).toBe(false);
    expect(isIconChoice(3)).toBe(false);
    expect(pngUrl("QUJD")).toBe("data:image/png;base64,QUJD");
  });
});
