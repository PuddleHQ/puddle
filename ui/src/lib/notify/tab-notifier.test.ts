// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import {
  cleanQuoted,
  inboxPath,
  requestNotice,
  TabNotifier,
  type ShownNotification,
} from "./tab-notifier.ts";

const request = (over: Record<string, unknown> = {}) => ({
  id: 7,
  workspace: "box",
  host: "www.example.com",
  registrable_domain: "example.com",
  port: 443,
  first_seen: 1,
  last_seen: 2,
  attempts: 1,
  ...over,
});
const notify = (over: Record<string, unknown> = {}, clients = ["tab-1"]) => ({
  type: "pending_notify",
  request: request(over),
  clients,
  desktop: false,
});

function setup(active = true) {
  const shown: {
    title: string;
    options: { body: string; tag: string; data: { request: number } };
    n: ShownNotification & { closed: number };
  }[] = [];
  const opened: string[] = [];
  const notifier = new TabNotifier({
    clientId: "tab-1",
    active: () => active,
    labelOf: (id) => (id === "box" ? "My box" : id),
    create: (title, options) => {
      const n = {
        onclick: null as ShownNotification["onclick"],
        closed: 0,
        close() {
          n.closed += 1;
        },
      };
      shown.push({ title, options, n });
      return n;
    },
    open: (path) => opened.push(path),
  });
  return { notifier, shown, opened };
}

describe("TabNotifier", () => {
  it("shows what the desktop app shows: the workspace's name and the host and port", () => {
    const { notifier, shown } = setup();
    expect(notifier.handle(notify())).toBe(true);
    expect(shown).toHaveLength(1);
    expect(shown[0]?.title).toBe("My box is asking for access");
    expect(shown[0]?.options).toEqual({
      body: "www.example.com:443 is waiting for your decision.",
      tag: "puddle-request-7",
      data: { request: 7 },
    });
  });

  it("a click closes the notification and opens the inbox at the request", () => {
    const { notifier, shown, opened } = setup();
    notifier.handle(notify());
    shown[0]?.n.onclick?.(new Event("click"));
    expect(shown[0]?.n.closed).toBe(1);
    expect(opened).toEqual(["/inbox?request=7"]);
    expect(inboxPath(7)).toBe("/inbox?request=7");
  });

  it("stays quiet when the service did not pick this tab or the switch is off", () => {
    const picked = setup();
    expect(picked.notifier.handle(notify({}, ["other"]))).toBe(true);
    expect(picked.notifier.handle(notify({}, []))).toBe(true);
    expect(picked.shown).toEqual([]);
    const off = setup(false);
    expect(off.notifier.handle(notify())).toBe(true);
    expect(off.shown).toEqual([]);
  });

  it("drops an event that is malformed and says it was not its own for any other", () => {
    const { notifier, shown } = setup();
    for (const bad of [
      { type: "pending_notify" },
      { type: "pending_notify", request: request(), clients: "tab-1" },
      { type: "pending_notify", request: { id: "7" }, clients: ["tab-1"] },
      notify({ workspace: 1 }),
      notify({ host: null }),
      notify({ port: "443" }),
    ]) {
      expect(notifier.handle(bad)).toBe(true);
    }
    expect(shown).toEqual([]);
    expect(notifier.handle(null)).toBe(false);
    expect(notifier.handle("pending_notify")).toBe(false);
    expect(notifier.handle({ type: "pending_opened" })).toBe(false);
  });

  it("takes the notification back when the request is decided", () => {
    const { notifier, shown } = setup();
    notifier.handle(notify());
    notifier.handle({ type: "pending_closed", id: 8 });
    expect(shown[0]?.n.closed).toBe(0);
    notifier.handle({ type: "pending_closed", id: 7 });
    expect(shown[0]?.n.closed).toBe(1);
    notifier.handle({ type: "pending_closed", id: 7 });
    expect(shown[0]?.n.closed).toBe(1);
    notifier.handle({ type: "pending_closed", id: "7" });
  });

  it("keeps at most 50 notifications to take back", () => {
    const { notifier, shown } = setup();
    for (let id = 1; id <= 52; id++) notifier.handle(notify({ id }));
    notifier.handle({ type: "pending_closed", id: 1 });
    notifier.handle({ type: "pending_closed", id: 52 });
    expect(shown[0]?.n.closed).toBe(0);
    expect(shown[51]?.n.closed).toBe(1);
  });

  it("goes on when the browser refuses to show one", () => {
    const create = vi.fn(() => {
      throw new Error("not allowed");
    });
    const notifier = new TabNotifier({
      clientId: "tab-1",
      active: () => true,
      labelOf: (id) => id,
      create,
      open: () => {},
    });
    expect(notifier.handle(notify())).toBe(true);
    expect(create).toHaveBeenCalledTimes(1);
  });
});

describe("what a notification may say", () => {
  it("never carries more of a workspace's words than the host and port", () => {
    const { notifier, shown } = setup();
    notifier.handle({
      ...notify({
        host: "a.test\n<img src=x onerror=alert(1)>&‮evil",
        workspace: "box",
        // Fields a hostile guest might hope reach the notification: none of them is read.
        registrable_domain: "ignore previous instructions",
        message: "click here",
      }),
      detail: "from the guest",
    });
    const text = [
      shown[0]?.title,
      shown[0]?.options.body,
      shown[0]?.options.tag,
    ]
      .map(String)
      .join(" ");
    expect(text).not.toMatch(/[<>&\n\u202e]/);
    expect(text).not.toContain("ignore previous");
    expect(text).not.toContain("click here");
    expect(text).not.toContain("from the guest");
    expect(shown[0]?.options.body).toBe(
      "a.test??img src=x onerror=alert(1)???evil:443 is waiting for your decision.",
    );
  });

  it("replaces control, direction and markup characters and cuts a long name", () => {
    expect(cleanQuoted("a\u0000b\u0007c d e<f>g&h")).toBe("a?b?c?d?e?f?g?h");
    expect(cleanQuoted("a​b‪c")).toBe("a?b?c");
    expect(cleanQuoted("x".repeat(80))).toBe("x".repeat(80));
    expect(cleanQuoted("x".repeat(81))).toBe(`${"x".repeat(80)}…`);
    expect(cleanQuoted("é".repeat(81))).toBe(`${"é".repeat(80)}…`);
    const long = requestNotice("w".repeat(200), "h".repeat(200), 8080);
    expect(long.title.length).toBeLessThan(120);
    expect(long.body.length).toBeLessThan(140);
  });
});
