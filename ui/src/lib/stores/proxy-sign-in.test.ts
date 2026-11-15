// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { report } from "#lib/testing/fake-network.ts";
import { ProxySignInStore } from "./proxy-sign-in.svelte.ts";

type Reply = {
  data?: unknown;
  error?: { message: string };
  response: { status: number; ok: boolean };
};

class Stub {
  signIns = [{ proxy: "a:1", user: "alice" }];
  health: unknown = null;
  calls: string[] = [];
  down = false;
  refuse: { status: number; message: string } | null = null;

  private answer(data: unknown, call: string): Reply {
    this.calls.push(call);
    if (this.down) throw new TypeError("fetch failed");
    if (this.refuse) {
      const { status, message } = this.refuse;
      return { error: { message }, response: { status, ok: false } };
    }
    return { data, response: { status: 200, ok: true } };
  }

  async GET(path: string) {
    if (path === "/api/network-health") {
      if (this.health === null) {
        return { response: { status: 503, ok: false } };
      }
      return this.answer(this.health, `GET ${path}`);
    }
    return this.answer({ sign_ins: this.signIns }, `GET ${path}`);
  }

  async PUT(path: string, init: { body: { proxy: string; user: string } }) {
    const reply = this.answer(
      { sign_ins: [{ proxy: init.body.proxy, user: init.body.user }] },
      `PUT ${path}`,
    );
    return reply;
  }

  async DELETE(path: string) {
    const reply = this.answer(undefined, `DELETE ${path}`);
    return reply.error
      ? reply
      : { ...reply, response: { status: 204, ok: true } };
  }
}

const make = () => {
  const stub = new Stub();
  return { stub, store: new ProxySignInStore(stub as never) };
};

describe("ProxySignInStore", () => {
  it("lists the stored sign-ins by proxy and user", async () => {
    const { store } = make();
    expect(store.status).toBe("loading");
    await store.load();
    expect(store.status).toBe("ready");
    expect(store.signIns).toEqual([{ proxy: "a:1", user: "alice" }]);
  });

  it("offers the proxies the network report names, each once, sorted", async () => {
    const { stub, store } = make();
    const r = report();
    r.proxy.http_proxy = "z:3128";
    r.proxy.https_proxy = "z:3128";
    r.proxy.dead_proxies = [{ proxy: "d:1", retry_in_secs: 5 }];
    r.sign_in.attempts = [
      {
        proxy: "b:8080",
        scheme: "Basic",
        result: "failed",
        detail: null,
        at: 1,
      },
    ];
    stub.health = r;
    await store.load();
    expect(store.suggestions).toEqual(["b:8080", "d:1", "z:3128"]);
  });

  it("offers nothing when the report can't be read, and still lists the sign-ins", async () => {
    const { store } = make();
    await store.load();
    expect(store.suggestions).toEqual([]);
    expect(store.status).toBe("ready");
  });

  it("offers nothing when the report read throws", async () => {
    const { stub, store } = make();
    stub.health = report();
    const get = stub.GET.bind(stub);
    stub.GET = async (path: string) => {
      if (path === "/api/network-health") throw new TypeError("down");
      return get(path);
    };
    await store.load();
    expect(store.suggestions).toEqual([]);
  });

  it("says why a read failed, and when the service does not answer", async () => {
    const { stub, store } = make();
    stub.refuse = { status: 500, message: "broken" };
    await store.load();
    expect(store.status).toBe("failed");
    expect(store.reason).toBe("Broken.");
    stub.refuse = null;
    stub.down = true;
    await store.load();
    expect(store.reason).toBe("Puddle's service isn't answering.");
  });

  it("stores a sign-in and shows what the API answers", async () => {
    const { stub, store } = make();
    await store.load();
    const result = await store.save("p:8080", "bob", "pw");
    expect(result).toEqual({ ok: true });
    expect(store.signIns).toEqual([{ proxy: "p:8080", user: "bob" }]);
    expect(stub.calls).toContain("PUT /api/proxy-sign-ins");
  });

  it("returns the refusal of a save, or that the service is down", async () => {
    const { stub, store } = make();
    stub.refuse = { status: 422, message: "the password is empty" };
    expect(await store.save("p:1", "a", "")).toEqual({
      ok: false,
      message: "The password is empty.",
    });
    stub.refuse = null;
    stub.down = true;
    expect(await store.save("p:1", "a", "x")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });

  it("forgets a sign-in, and reports a refusal or a service that is down", async () => {
    const { stub, store } = make();
    await store.load();
    expect(await store.remove("a:1")).toEqual({ ok: true });
    expect(store.signIns).toEqual([]);
    stub.refuse = { status: 503, message: "store locked" };
    expect(await store.remove("a:1")).toEqual({
      ok: false,
      message: "Store locked.",
    });
    stub.refuse = null;
    stub.down = true;
    expect(await store.remove("a:1")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });

  it("ignores a failed read that a newer call has overtaken", async () => {
    const { stub, store } = make();
    stub.down = true;
    const first = store.load();
    await store.save("p:1", "x", "y").catch(() => undefined);
    await first;
    expect(store.status).toBe("loading");
  });

  it("ignores a read that a newer call has overtaken", async () => {
    const { stub, store } = make();
    const first = store.load();
    await store.save("p:1", "x", "y");
    await first;
    expect(store.signIns).toEqual([{ proxy: "p:1", user: "x" }]);
    void stub;
  });
});
