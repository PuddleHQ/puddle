// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import { KeptLogins } from "./kept-logins.svelte.ts";

type Reply = { data?: unknown; error?: { message: string }; status?: number };

/** An API whose answers the test scripts, one per call, in order. */
function scripted(get: Reply[], del: Reply[] = []) {
  const reply = (r: Reply | undefined) => {
    const status = r?.status ?? (r?.error ? 500 : 200);
    return { data: r?.data, error: r?.error, response: { status } };
  };
  return {
    GET: vi.fn(async () => reply(get.shift())),
    DELETE: vi.fn(async () => reply(del.shift() ?? { status: 204 })),
  };
}

const claude = { service: "claude", name: "Claude Code", since: 1 };
const github = { service: "github", name: "GitHub", since: null };

describe("kept logins", () => {
  it("reads the list and whether the store could be read", async () => {
    const api = scripted([
      { data: { logins: [claude, github], store_readable: true } },
    ]);
    const kept = new KeptLogins(api as never);
    expect(kept.status).toBe("loading");
    await kept.load("demo");
    expect(kept.status).toBe("ready");
    expect(kept.logins.map((l) => l.service)).toEqual(["claude", "github"]);
    expect(kept.storeReadable).toBe(true);
    expect(api.GET).toHaveBeenCalledWith("/api/workspaces/{id}/logins", {
      params: { path: { id: "demo" } },
    });
  });

  it("keeps an unreadable store apart from an empty list", async () => {
    const kept = new KeptLogins(
      scripted([{ data: { logins: [], store_readable: false } }]) as never,
    );
    await kept.load("demo");
    expect(kept.status).toBe("ready");
    expect(kept.storeReadable).toBe(false);
    expect(kept.logins).toEqual([]);
  });

  it("fails with the API's words, with no service, and a quiet reload keeps what is shown", async () => {
    const kept = new KeptLogins(
      scripted([
        { error: { message: "no such workspace" }, status: 404 },
        { data: { logins: [claude], store_readable: true } },
        { error: { message: "boom" } },
      ]) as never,
    );
    await kept.load("demo");
    expect(kept.status).toBe("failed");
    expect(kept.reason).toBe("No such workspace.");
    await kept.load("demo");
    await kept.load("demo", true);
    expect(kept.status).toBe("ready");
    expect(kept.logins).toHaveLength(1);

    const down = new KeptLogins({
      GET: async () => {
        throw new TypeError("down");
      },
      DELETE: vi.fn(),
    } as never);
    await down.load("demo");
    expect(down.status).toBe("failed");
    expect(down.reason).toBe("Puddle's service isn't answering.");
  });

  it("forgets a service by its short name and drops the row only once the API says it is gone", async () => {
    const api = scripted(
      [{ data: { logins: [claude, github], store_readable: true } }],
      [
        {
          error: {
            message:
              "the credential store did not delete a saved login; unlock it",
          },
        },
        { status: 204 },
      ],
    );
    const kept = new KeptLogins(api as never);
    await kept.load("demo");
    const refused = await kept.forget("claude");
    expect(refused).toEqual({
      ok: false,
      message: "The credential store did not delete a saved login; unlock it.",
    });
    expect(kept.logins).toHaveLength(2);
    expect(await kept.forget("claude")).toEqual({ ok: true });
    expect(kept.logins.map((l) => l.service)).toEqual(["github"]);
    expect(api.DELETE).toHaveBeenLastCalledWith(
      "/api/workspaces/{id}/logins/{service}",
      { params: { path: { id: "demo", service: "claude" } } },
    );
  });

  it("counts a login that is already gone as forgotten, and says when the service does not answer", async () => {
    const api = scripted(
      [{ data: { logins: [claude], store_readable: true } }],
      [{ error: { message: "no login service" }, status: 404 }],
    );
    const kept = new KeptLogins(api as never);
    await kept.load("demo");
    expect(await kept.forget("claude")).toEqual({ ok: true });
    expect(kept.logins).toEqual([]);
    api.DELETE.mockRejectedValueOnce(new TypeError("down"));
    const down = await kept.forget("github");
    expect(down).toEqual({
      ok: false,
      message: "Puddle's service isn't answering. Try again.",
    });
  });

  it("does not let a read that began before a forget bring the row back", async () => {
    let release: (value: unknown) => void = () => {};
    const slow = new Promise((resolve) => (release = resolve));
    const api = {
      GET: vi
        .fn()
        .mockResolvedValueOnce({
          data: { logins: [claude], store_readable: true },
          response: { status: 200 },
        })
        .mockImplementationOnce(() => slow),
      DELETE: vi.fn(async () => ({ response: { status: 204 } })),
    };
    const kept = new KeptLogins(api as never);
    await kept.load("demo");
    const reading = kept.load("demo", true);
    await kept.forget("claude");
    release({
      data: { logins: [claude], store_readable: true },
      response: { status: 200 },
    });
    await reading;
    expect(kept.logins).toEqual([]);
  });
});
