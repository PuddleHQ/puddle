// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PresenceReporter, REPORT_EVERY_MS } from "./reporter.ts";

class FakeTarget {
  visibilityState: DocumentVisibilityState = "visible";
  readonly #listeners = new Map<string, Set<() => void>>();
  addEventListener(type: string, listener: () => void) {
    const set = this.#listeners.get(type) ?? new Set();
    set.add(listener);
    this.#listeners.set(type, set);
  }
  removeEventListener(type: string, listener: () => void) {
    this.#listeners.get(type)?.delete(listener);
  }
  fire(type: string) {
    for (const l of this.#listeners.get(type) ?? []) l();
  }
  count() {
    return [...this.#listeners.values()].reduce((n, s) => n + s.size, 0);
  }
}

interface Body {
  client: string;
  kind: string;
  visible: boolean;
  idle_ms: number;
  notify: boolean;
}

function setup(over: { notify?: () => boolean; fail?: boolean } = {}) {
  const doc = new FakeTarget();
  const win = new FakeTarget();
  const sent: { body: Body; keepalive?: boolean }[] = [];
  let clock = 1_000_000;
  const api = {
    POST: vi.fn(
      async (_path: string, init: { body: Body; keepalive?: boolean }) => {
        if (over.fail) throw new Error("offline");
        sent.push(init);
        return { response: { ok: true } };
      },
    ),
  };
  const reporter = new PresenceReporter({
    api: api as never,
    clientId: "tab-1",
    kind: "browser",
    notify: over.notify ?? (() => true),
    document: doc,
    window: win,
    now: () => clock,
  });
  return {
    doc,
    win,
    sent,
    api,
    reporter,
    advance: (ms: number) => (clock += ms),
  };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("PresenceReporter", () => {
  it("reports at once and then every 30 seconds, with the time since the last input", async () => {
    const t = setup();
    const stop = t.reporter.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.api.POST.mock.calls[0]?.[0]).toBe("/api/presence");
    expect(t.sent[0]?.body).toEqual({
      client: "tab-1",
      kind: "browser",
      visible: true,
      idle_ms: 0,
      notify: true,
    });
    t.advance(20_000);
    t.doc.fire("keydown");
    t.advance(5_000);
    await vi.advanceTimersByTimeAsync(REPORT_EVERY_MS);
    expect(t.sent).toHaveLength(2);
    expect(t.sent[1]?.body.idle_ms).toBe(5_000);
    stop();
    await vi.advanceTimersByTimeAsync(REPORT_EVERY_MS * 3);
    expect(t.sent).toHaveLength(2);
  });

  it("reports when the page goes to the background or comes back, and not for a change that is nothing", async () => {
    const t = setup();
    const stop = t.reporter.start();
    await vi.advanceTimersByTimeAsync(0);
    t.win.fire("focus");
    await vi.advanceTimersByTimeAsync(0);
    expect(t.sent).toHaveLength(1);
    t.doc.visibilityState = "hidden";
    t.doc.fire("visibilitychange");
    await vi.advanceTimersByTimeAsync(0);
    expect(t.sent.map((s) => s.body.visible)).toEqual([true, false]);
    t.doc.visibilityState = "visible";
    t.win.fire("blur");
    await vi.advanceTimersByTimeAsync(0);
    expect(t.sent.map((s) => s.body.visible)).toEqual([true, false, true]);
    stop();
  });

  it("reports when the switch changes", async () => {
    let on = false;
    const t = setup({ notify: () => on });
    t.reporter.start();
    await vi.advanceTimersByTimeAsync(0);
    on = true;
    t.reporter.refresh();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.sent.map((s) => s.body.notify)).toEqual([false, true]);
  });

  it("refreshes before anything was sent by reporting", async () => {
    const t = setup();
    t.reporter.refresh();
    await vi.advanceTimersByTimeAsync(0);
    expect(t.sent).toHaveLength(1);
  });

  it("says it is leaving when the page is hidden for good, and reports again if it comes back", async () => {
    const t = setup();
    t.reporter.start();
    await vi.advanceTimersByTimeAsync(0);
    t.win.fire("pagehide");
    await vi.advanceTimersByTimeAsync(0);
    const last = t.sent.at(-1);
    expect(last?.keepalive).toBe(true);
    expect(last?.body).toMatchObject({ visible: false, notify: false });
    expect(last?.body.idle_ms).toBeGreaterThan(600_000);
    t.win.fire("pageshow");
    await vi.advanceTimersByTimeAsync(0);
    expect(t.sent.at(-1)?.body).toMatchObject({ visible: true, notify: true });
  });

  it("ignores a report the service did not take", async () => {
    const t = setup({ fail: true });
    const stop = t.reporter.start();
    await vi.advanceTimersByTimeAsync(REPORT_EVERY_MS);
    expect(t.api.POST).toHaveBeenCalledTimes(2);
    stop();
  });

  it("removes every listener it added when stopped", async () => {
    const t = setup();
    const stop = t.reporter.start();
    expect(t.doc.count() + t.win.count()).toBeGreaterThan(0);
    stop();
    expect(t.doc.count() + t.win.count()).toBe(0);
  });

  it("listens to the real document and window by default", async () => {
    const api = { POST: vi.fn(async () => ({ response: { ok: true } })) };
    const reporter = new PresenceReporter({
      api: api as never,
      clientId: "tab-2",
      kind: "desktop",
      notify: () => true,
    });
    const stop = reporter.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(api.POST).toHaveBeenCalledTimes(1);
    stop();
  });
});
