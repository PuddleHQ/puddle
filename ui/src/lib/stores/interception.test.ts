// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import type { components } from "#lib/api/schema.d.ts";
import {
  GlobalNeverDecrypt,
  WorkspaceInterception,
} from "./interception.svelte.ts";

type View = components["schemas"]["InterceptionView"];

/** The API's interception routes in the shape openapi-fetch returns. */
class Stub {
  view: View = {
    decrypt_all: false,
    global_never_decrypt: ["bank.example"],
    never_decrypt: [],
    workspace: "demo" as View["workspace"],
  };
  global = { never_decrypt: ["bank.example"] };
  calls: string[] = [];
  bodies: unknown[] = [];
  down = false;
  refuse: { status: number; message: string } | null = null;

  private answer<T>(data: T, path: string, body?: unknown) {
    this.calls.push(path);
    if (body !== undefined) this.bodies.push(body);
    if (this.down) throw new TypeError("fetch failed");
    if (this.refuse) {
      const { status, message } = this.refuse;
      return {
        data: undefined,
        error: { message },
        response: { status },
      };
    }
    return { data, error: undefined, response: { status: 200 } };
  }

  async GET(path: string) {
    return path === "/api/interception/never-decrypt"
      ? this.answer(this.global, `GET ${path}`)
      : this.answer(this.view, `GET ${path}`);
  }

  async PUT(
    path: string,
    init: { body: { decrypt_all?: boolean; never_decrypt?: string[] } },
  ) {
    if (this.refuse === null) {
      if (path === "/api/interception/never-decrypt") {
        this.global = { never_decrypt: init.body.never_decrypt ?? [] };
      } else {
        this.view = {
          ...this.view,
          decrypt_all: init.body.decrypt_all ?? this.view.decrypt_all,
          never_decrypt: init.body.never_decrypt ?? this.view.never_decrypt,
        };
      }
    }
    return this.answer(
      path === "/api/interception/never-decrypt" ? this.global : this.view,
      `PUT ${path}`,
      init.body,
    );
  }
}

let api: Stub;
beforeEach(() => {
  api = new Stub();
});

describe("WorkspaceInterception", () => {
  it("reads the switch and both lists", async () => {
    const store = new WorkspaceInterception(api as never);
    expect(store.status).toBe("loading");
    await store.load("ws-1");
    expect(store.status).toBe("ready");
    expect(store.decryptAll).toBe(false);
    expect(store.globalNames).toEqual(["bank.example"]);
    expect(api.calls).toEqual(["GET /api/workspaces/{id}/interception"]);
  });

  it("sends only the switch when it is flipped, and shows what the API answers", async () => {
    const store = new WorkspaceInterception(api as never);
    await store.load("ws-1");
    expect(await store.setDecryptAll(true)).toEqual({ ok: true });
    expect(api.bodies).toEqual([{ decrypt_all: true }]);
    expect(store.decryptAll).toBe(true);
  });

  it("sends only the names when they are saved", async () => {
    const store = new WorkspaceInterception(api as never);
    await store.load("ws-1");
    await store.saveNames(["a.io"]);
    expect(api.bodies).toEqual([{ never_decrypt: ["a.io"] }]);
    expect(store.names).toEqual(["a.io"]);
  });

  it("keeps the old state and says why when the API refuses a name", async () => {
    const store = new WorkspaceInterception(api as never);
    await store.load("ws-1");
    api.refuse = { status: 422, message: "10.0.0.1 is an IP address" };
    const result = await store.saveNames(["10.0.0.1"]);
    expect(result).toEqual({
      ok: false,
      message: "10.0.0.1 is an IP address.",
    });
    expect(store.names).toEqual([]);
  });

  it("fails to load without a service and with a refusal", async () => {
    const store = new WorkspaceInterception(api as never);
    api.down = true;
    await store.load("ws-1");
    expect(store.status).toBe("failed");
    expect(store.reason).toBe("Puddle's service isn't answering.");
    api.down = false;
    api.refuse = { status: 404, message: "no such workspace" };
    await store.load("ws-1");
    expect(store.reason).toBe("No such workspace.");
    expect(await store.setDecryptAll(true)).toEqual({
      ok: false,
      message: "No such workspace.",
    });
  });

  it("a save when the service is down reports it", async () => {
    const store = new WorkspaceInterception(api as never);
    await store.load("ws-1");
    api.down = true;
    expect(await store.setDecryptAll(true)).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });
});

describe("GlobalNeverDecrypt", () => {
  it("reads and replaces the list", async () => {
    const store = new GlobalNeverDecrypt(api as never);
    await store.load();
    expect(store.status).toBe("ready");
    expect(store.names).toEqual(["bank.example"]);
    expect(await store.save(["x.io", "y.io"])).toEqual({ ok: true });
    expect(api.bodies).toEqual([{ never_decrypt: ["x.io", "y.io"] }]);
    expect(store.names).toEqual(["x.io", "y.io"]);
  });

  it("fails to load and to save", async () => {
    const store = new GlobalNeverDecrypt(api as never);
    api.down = true;
    await store.load();
    expect(store.status).toBe("failed");
    api.down = false;
    await store.load();
    api.refuse = { status: 422, message: "too many names" };
    expect(await store.save(["x.io"])).toEqual({
      ok: false,
      message: "Too many names.",
    });
    expect(store.names).toEqual(["bank.example"]);
  });
});
