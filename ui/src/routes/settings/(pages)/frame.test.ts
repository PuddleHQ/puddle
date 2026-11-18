// SPDX-License-Identifier: AGPL-3.0-or-later
// The frame of Settings: the list of pages, the search, and the states before the settings load.
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = await vi.hoisted(async () => {
  const mod = await import("#lib/testing/fake-settings.ts");
  return new mod.FakeSettings();
});
vi.mock("#lib/api/client.ts", () => ({ api }));
const location = vi.hoisted(() => ({ path: "/settings/notifications" }));
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(`http://127.0.0.1${location.path}`);
    },
    status: 200,
    error: null,
  },
}));

import { PAGES, SETTINGS } from "#lib/settings/pages.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import SettingsHost from "#lib/testing/SettingsHost.svelte";
import Appearance from "./appearance/+page.svelte";
import EditorAndGit from "./editor-and-git/+page.svelte";
import Network from "./network/+page.svelte";
import Notifications from "./notifications/+page.svelte";
import ThisComputer from "./this-computer/+page.svelte";
import Workspaces from "./workspaces/+page.svelte";

const BY_PAGE = {
  appearance: Appearance,
  notifications: Notifications,
  workspaces: Workspaces,
  network: Network,
  "editor-and-git": EditorAndGit,
  "this-computer": ThisComputer,
} as const;

beforeEach(() => {
  api.reset();
  location.path = "/settings/notifications";
  globalSettings.view = null;
  globalSettings.consents = null;
  globalSettings.status = "loading";
  globalSettings.reason = null;
  localStorage.clear();
});
afterEach(cleanup);

const open = async (pages: (typeof BY_PAGE)[keyof typeof BY_PAGE][]) => {
  render(SettingsHost, { props: { pages } });
  await screen.findByRole("navigation", { name: "Settings pages" });
};

describe("the list of pages", () => {
  it("lists the six pages and marks the one that is open", async () => {
    await open([Notifications]);
    const nav = screen.getByRole("navigation", { name: "Settings pages" });
    const links = within(nav).getAllByRole("link");
    expect(links.map((l) => l.textContent)).toEqual([
      "Appearance",
      "Notifications",
      "Workspaces",
      "Network",
      "Editor and Git",
      "This computer",
    ]);
    expect(links.map((l) => l.getAttribute("href"))).toEqual(
      PAGES.map((p) => p.href),
    );
    expect(
      within(nav).getByRole("link", { name: "Notifications" }),
    ).toHaveAttribute("aria-current", "page");
    expect(
      within(nav).getByRole("link", { name: "Network" }),
    ).not.toHaveAttribute("aria-current");
  });

  it("shows one page at a time with its own heading and has no page-wide Save bar", async () => {
    await open([Notifications]);
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    expect(
      screen.getByRole("heading", { level: 2, name: "Notifications" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Appearance" })).toBeNull();
    expect(screen.queryByRole("group", { name: "Save settings" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Discard" })).toBeNull();
  });
});

describe("the search", () => {
  it("lists the settings that match, each opening its page at the setting", async () => {
    await open([Notifications]);
    await fireEvent.input(
      screen.getByRole("searchbox", { name: "Search settings" }),
      { target: { value: "zoom" } },
    );
    const results = screen.getByRole("region", { name: "Search results" });
    const link = within(results).getByRole("link", { name: /Zoom shortcuts/ });
    expect(link).toHaveAttribute("href", "/settings/editor-and-git#set-zoom");
    expect(link).toHaveTextContent("Editor and Git");
    expect(
      screen.queryByRole("navigation", { name: "Settings pages" }),
    ).toBeNull();
  });

  it("says when nothing matches, and the list comes back when the search is cleared", async () => {
    await open([Notifications]);
    const box = screen.getByRole("searchbox", { name: "Search settings" });
    await fireEvent.input(box, { target: { value: "zzzz" } });
    expect(screen.getByText(/No setting matches "zzzz"/)).toBeInTheDocument();
    await fireEvent.input(box, { target: { value: "" } });
    expect(
      screen.getByRole("navigation", { name: "Settings pages" }),
    ).toBeInTheDocument();
  });

  it("clears itself when a result is chosen", async () => {
    await open([Notifications]);
    const box = screen.getByRole("searchbox", {
      name: "Search settings",
    }) as HTMLInputElement;
    await fireEvent.input(box, { target: { value: "sound" } });
    await fireEvent.click(screen.getByRole("link", { name: /^Sound/ }));
    expect(box.value).toBe("");
  });
});

describe("every setting is reachable by search", () => {
  it("has each searchable setting on its page, under the id the search links to", async () => {
    const pages = Object.values(BY_PAGE);
    await open(pages);
    // Shown only while Microsoft's server is chosen, or once it was accepted.
    const conditional = new Set(["set-ms-telemetry"]);
    const missing = SETTINGS.filter(
      (s) => !conditional.has(s.id) && document.getElementById(s.id) === null,
    ).map((s) => `${s.page}#${s.id}`);
    expect(missing).toEqual([]);
  });
});

describe("before the settings load", () => {
  it("says when the settings can't be read, and when a newer Puddle wrote them", async () => {
    api.down = true;
    render(SettingsHost, { props: { pages: [Appearance] } });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Couldn't read Puddle's settings.",
    );
    cleanup();
    api.down = false;
    globalSettings.status = "loading";
    api.refuse.set("GET /api/settings", {
      status: 409,
      error: "newer_settings",
      message: "newer",
    });
    render(SettingsHost, { props: { pages: [Appearance] } });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /newer Puddle\. Update Puddle/,
    );
  });

  it("shows why the settings can't be read when the API says", async () => {
    api.refuse.set("GET /api/settings", {
      status: 409,
      error: "settings_unreadable",
      message:
        "Puddle cannot read the global settings: global settings must be an object",
    });
    render(SettingsHost, { props: { pages: [Appearance] } });
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("global settings must be an object");
  });

  it("lists the stored fields it doesn't know, and nothing when there are none", async () => {
    await open([Appearance]);
    expect(screen.queryByText(/fields this Puddle doesn't know/)).toBeNull();
    cleanup();
    globalSettings.view = null;
    globalSettings.status = "loading";
    api.unknown = ["workspace_defaults.memry", "color"];
    await open([Appearance]);
    expect(
      screen.getByText(/fields this Puddle doesn't know/),
    ).toHaveTextContent("workspace_defaults.memry, color");
  });
});
