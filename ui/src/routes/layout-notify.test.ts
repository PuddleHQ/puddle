// SPDX-License-Identifier: AGPL-3.0-or-later
// What the page shell does about being told of a new request: it reports its presence, and a
// tab the service picked shows a Web Notification; the desktop window never does.
import { cleanup, render } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const url = vi.hoisted(() => ({ pathname: "/workspaces" }));
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(`http://127.0.0.1${url.pathname}`);
    },
    status: 200,
    error: null,
  },
}));
const nav = vi.hoisted(() => ({ goto: vi.fn(async () => undefined) }));
vi.mock("$app/navigation", () => nav);

const api = vi.hoisted(() => ({
  POST: vi.fn(async () => ({ response: { ok: true } })),
  GET: vi.fn(async () => ({ data: undefined, response: { ok: false } })),
  PUT: vi.fn(),
  DELETE: vi.fn(),
}));
vi.mock("#lib/api/client.ts", () => ({ api }));

const live = vi.hoisted(() => ({
  pending: 0,
  asks: 0,
  open: 0,
  problem: null,
  stream: "connected",
  start: vi.fn(() => () => undefined),
  listener: null as null | { event?: (e: unknown) => void },
  subscribe: vi.fn(function (l: { event?: (e: unknown) => void }) {
    live.listener = l;
    return () => undefined;
  }),
}));
vi.mock("#lib/stores/live.svelte.ts", () => ({ live }));
vi.mock("#lib/stores/network-health.svelte.ts", () => ({
  networkHealth: { report: null, start: vi.fn(() => () => undefined) },
}));
vi.mock("#lib/stores/admin.svelte.ts", () => ({
  admin: { available: false, probe: vi.fn(async () => undefined) },
}));

import Layout from "./+layout.svelte";
import { tabNotifications } from "#lib/notify/tab-switch.svelte.ts";
import { clientId } from "#lib/presence/client-id.ts";
import { session } from "#lib/session/session.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";

const shown: { title: string; options: { body: string; tag: string } }[] = [];
class FakeNotification {
  static permission = "granted";
  static requestPermission = async () => "granted";
  onclick: (() => void) | null = null;
  constructor(title: string, options: { body: string; tag: string }) {
    shown.push({ title, options });
    instances.push(this);
  }
  close() {}
}
const instances: FakeNotification[] = [];

const bodies = () =>
  (
    api.POST.mock.calls as unknown as [
      string,
      { body: Record<string, unknown> },
    ][]
  ).map((c) => c[1].body);

beforeEach(() => {
  shown.length = 0;
  instances.length = 0;
  api.POST.mockClear();
  nav.goto.mockClear();
  live.listener = null;
  (globalThis as { __PUDDLE__?: unknown }).__PUDDLE__ = { token: "shell" };
  session.mode = "local";
  sessionStorage.clear();
  localStorage.clear();
  tabNotifications.chosen = false;
  vi.stubGlobal("Notification", FakeNotification);
  tabNotifications.refresh();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  delete (globalThis as { __PUDDLE__?: unknown }).__PUDDLE__;
});

const notify = (clients: string[]) => ({
  type: "pending_notify",
  clients,
  desktop: false,
  request: {
    id: 9,
    workspace: "box",
    host: "evil.example\n<b>",
    registrable_domain: "example",
    port: 443,
    first_seen: 1,
    last_seen: 1,
    attempts: 1,
  },
});

describe("the page shell and notifications", () => {
  it("reports this tab to the service, with its own id and its switch", async () => {
    render(Layout, { children: (() => {}) as never });
    await vi.waitFor(() => expect(api.POST).toHaveBeenCalled());
    expect(bodies()[0]).toMatchObject({
      client: clientId(),
      kind: "browser",
      visible: true,
      notify: false,
    });
  });

  it("reports again at once when the switch is turned on", async () => {
    render(Layout, { children: (() => {}) as never });
    await vi.waitFor(() => expect(api.POST).toHaveBeenCalledTimes(1));
    tabNotifications.chosen = true;
    await vi.waitFor(() => expect(api.POST).toHaveBeenCalledTimes(2));
    expect(bodies()[1]).toMatchObject({ notify: true });
  });

  it("shows a notification only when the service named this tab, and a click opens the inbox", async () => {
    tabNotifications.chosen = true;
    workspaces.labelOf = () => "My box";
    render(Layout, { children: (() => {}) as never });
    await vi.waitFor(() => expect(live.listener).not.toBeNull());
    live.listener?.event?.(notify(["someone-else"]));
    expect(shown).toEqual([]);
    live.listener?.event?.(notify([clientId()]));
    expect(shown).toHaveLength(1);
    expect(shown[0]?.title).toBe("My box is asking for access");
    expect(shown[0]?.options.body).toBe(
      "evil.example??b?:443 is waiting for your decision.",
    );
    expect(shown[0]?.options.tag).toBe("puddle-request-9");
    const focus = vi.spyOn(window, "focus").mockImplementation(() => undefined);
    instances[0]?.onclick?.();
    expect(focus).toHaveBeenCalled();
    expect(nav.goto).toHaveBeenCalledWith("/inbox?request=9");
  });

  it("is the desktop window's own client: reports as desktop and shows no web notification", async () => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    tabNotifications.chosen = true;
    render(Layout, { children: (() => {}) as never });
    await vi.waitFor(() => expect(api.POST).toHaveBeenCalled());
    expect(bodies()[0]).toMatchObject({ kind: "desktop", notify: true });
    live.listener?.event?.(notify([clientId()]));
    expect(shown).toEqual([]);
  });
});
