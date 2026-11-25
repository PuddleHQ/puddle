// SPDX-License-Identifier: AGPL-3.0-or-later
// The restore calls against a stand-in service that sees the raw request.
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
  applyRestore,
  deleteSafetyCopy,
  differing,
  dismissResult,
  droppedByReplace,
  loadResult,
  loadSafetyCopies,
  partLabel,
  previewRestore,
  safetyCopyTime,
  toAdd,
  type RestorePart,
} from "./restore.ts";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });

beforeEach(() => {
  wire.handler = () => Promise.reject(new TypeError("down"));
});

const part = (
  name: RestorePart["name"],
  over: Partial<RestorePart> = {},
): RestorePart => ({
  name,
  in_backup: 0,
  add: 0,
  differ: 0,
  same: 0,
  only_here: 0,
  ...over,
});

describe("previewing a file", () => {
  it("sends the file bytes as they are, with the key, and returns the preview", async () => {
    let seen: { method: string; type: string | null; body: string } | null =
      null;
    wire.handler = async (request) => {
      seen = {
        method: request.method,
        type: request.headers.get("Content-Type"),
        body: await request.text(),
      };
      return json({ staging_id: "ab".repeat(16), parts: [] });
    };
    const result = await previewRestore(
      new File(["file bytes"], "backup.tar.gz"),
    );
    expect(seen).toEqual({
      method: "POST",
      type: "application/gzip",
      body: "file bytes",
    });
    expect(result.ok && result.value.staging_id).toBe("ab".repeat(16));
  });

  it("names the reason when the file is refused", async () => {
    wire.handler = async () =>
      json(
        {
          error: "invalid",
          message:
            "made by a newer Puddle, schema 40; this one reads up to 32: update Puddle first",
        },
        422,
      );
    const result = await previewRestore(new File(["x"], "b.tar.gz"));
    expect(result).toEqual({
      ok: false,
      reason:
        "Made by a newer Puddle, schema 40; this one reads up to 32: update Puddle first.",
    });
  });

  it("says the service is not answering when it cannot be reached", async () => {
    expect(await previewRestore(new File(["x"], "b"))).toEqual({
      ok: false,
      reason: "Puddle's service isn't answering.",
    });
  });
});

describe("confirming", () => {
  it("posts the staging id and the mode", async () => {
    let body: unknown = null;
    wire.handler = async (request) => {
      body = await request.json();
      return json({ safety_copy: "20261115T225500Z", restart_required: true });
    };
    expect(await applyRestore("0".repeat(32), "replace")).toEqual({
      ok: true,
      value: { safetyCopy: "20261115T225500Z", restartRequired: true },
    });
    expect(body).toEqual({ staging_id: "0".repeat(32), mode: "replace" });
  });

  it("says why a restore did not start", async () => {
    wire.handler = async () =>
      json(
        {
          error: "unavailable",
          message:
            "the safety copy of the present state could not be made, so the restore did not start",
        },
        503,
      );
    const result = await applyRestore("0".repeat(32), "add");
    expect(result.ok).toBe(false);
    expect(!result.ok && result.reason).toContain("did not start");
  });
});

describe("the result and the safety copies", () => {
  it("reads the last result and clears it", async () => {
    wire.handler = async (request) =>
      request.method === "DELETE"
        ? new Response(null, { status: 204 })
        : json({
            result: {
              applied: true,
              mode: "add",
              safety_copy: "20261115T225500Z",
              at_ms: 1,
              message: "Restored.",
            },
          });
    const read = await loadResult();
    expect(read.ok && read.value?.message).toBe("Restored.");
    expect(await dismissResult()).toEqual({ ok: true, value: null });
  });

  it("lists the copies and deletes one by name", async () => {
    let path = "";
    wire.handler = async (request) => {
      path = `${request.method} ${new URL(request.url).pathname}`;
      return request.method === "DELETE"
        ? new Response(null, { status: 204 })
        : json({ copies: [{ name: "20261115T225500Z", size_bytes: 12 }] });
    };
    const list = await loadSafetyCopies();
    expect(list.ok && list.value[0]?.size_bytes).toBe(12);
    expect(path).toBe("GET /api/safety-copies");
    expect(await deleteSafetyCopy("20261115T225500Z")).toEqual({
      ok: true,
      value: null,
    });
    expect(path).toBe("DELETE /api/safety-copies/20261115T225500Z");
  });

  it("reports a delete that fails", async () => {
    wire.handler = async () =>
      json({ error: "not_found", message: "no safety copy named x" }, 404);
    expect(await deleteSafetyCopy("x")).toEqual({
      ok: false,
      reason: "No safety copy named x.",
    });
  });
});

describe("what the preview says", () => {
  const parts = [
    part("rules", { add: 2, differ: 1, only_here: 3 }),
    part("workspaces", { add: 1, only_here: 1 }),
    part("users", { add: 5, differ: 4, only_here: 9 }),
  ];
  it("counts what is added over every part", () => {
    expect(toAdd(parts)).toBe(8);
  });
  it("leaves the users out of what Replace changes, since a restore never replaces them", () => {
    expect(differing(parts)).toBe(1);
    expect(droppedByReplace(parts)).toBe(4);
  });
  it("labels the parts and reads a safety copy's name as a time", () => {
    expect(partLabel("rule_sets")).toBe("Rule sets");
    expect(safetyCopyTime("20261115T225500Z")).toBe("2026-11-15 22:55 UTC");
    expect(safetyCopyTime("odd")).toBe("odd");
  });
});

describe("every call says so when the service cannot be reached or refuses", () => {
  const calls: [string, () => Promise<{ ok: boolean }>][] = [
    ["confirming", () => applyRestore("0".repeat(32), "add")],
    ["reading the result", () => loadResult()],
    ["clearing the result", () => dismissResult()],
    ["listing the safety copies", () => loadSafetyCopies()],
    ["deleting a safety copy", () => deleteSafetyCopy("x")],
  ];
  it.each(calls)("%s: down", async (_name, call) => {
    expect(await call()).toEqual({
      ok: false,
      reason: "Puddle's service isn't answering.",
    });
  });
  it.each(calls)("%s: refused with a reason", async (_name, call) => {
    wire.handler = async () =>
      json({ error: "internal", message: "disk is full" }, 500);
    expect(await call()).toEqual({ ok: false, reason: "Disk is full." });
  });
});
