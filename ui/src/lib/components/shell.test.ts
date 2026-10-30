// SPDX-License-Identifier: GPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const url = vi.hoisted(() => ({ pathname: "/inbox" }));
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(`http://127.0.0.1${url.pathname}`);
    },
    status: 404,
    error: null,
  },
}));

const live = vi.hoisted(() => ({
  pending: 0,
  problem: null as null | "unauthorized" | "unreachable" | "failing",
  problemDetail: null as string | null,
  streamError: null as string | null,
  streamRefusal: null as null | { reason: string; advice: string },
  stream: "connected" as string,
  start: vi.fn(() => () => undefined),
  subscribe: vi.fn(() => () => undefined),
}));
vi.mock("#lib/stores/live.svelte.ts", () => ({ live }));

const health = vi.hoisted(() => ({
  report: null,
  start: vi.fn(() => () => undefined),
}));
vi.mock("#lib/stores/network-health.svelte.ts", () => ({
  networkHealth: health,
}));

import Layout from "../../routes/+layout.svelte";
import ErrorPage from "../../routes/+error.svelte";
import { ssr, prerender } from "../../routes/+layout.ts";
import Sidebar from "./Sidebar.svelte";
import { theme } from "../theme/theme.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";

beforeEach(() => {
  url.pathname = "/inbox";
  live.pending = 0;
  live.problem = null;
  live.problemDetail = null;
  live.streamError = null;
  live.streamRefusal = null;
  live.stream = "connected";
  live.start.mockClear();
  live.subscribe.mockClear();
  health.start.mockClear();
  localStorage.clear();
  delete document.documentElement.dataset["theme"];
  // The theme is one object for the whole file: read the (now empty) storage so every test starts on "system".
  theme.init();
});
afterEach(() => {
  vi.restoreAllMocks();
  cleanup();
});

describe("Sidebar", () => {
  it("lists the six sections and marks the current one", () => {
    url.pathname = "/rules/abc";
    render(Sidebar);
    const links = screen.getAllByRole("link");
    expect(links.map((l) => l.textContent?.trim())).toEqual([
      "Workspaces",
      "Inbox",
      "Rules",
      "Activity",
      "Identities",
      "Settings",
    ]);
    expect(screen.getByRole("link", { name: "Rules" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(screen.getByRole("link", { name: "Inbox" })).not.toHaveAttribute(
      "aria-current",
    );
    expect(
      screen.getByRole("navigation", { name: "Main" }),
    ).toBeInTheDocument();
  });

  it("shows the pending badge with an accessible name, only when there is something pending", () => {
    const { unmount } = render(Sidebar);
    expect(screen.queryByTestId("pending-badge")).toBeNull();
    unmount();
    live.pending = 4;
    render(Sidebar);
    expect(screen.getByTestId("pending-badge")).toHaveTextContent(
      "4 4 pending",
    );
    expect(
      screen.getByRole("link", { name: "Inbox 4 pending" }),
    ).toBeInTheDocument();
  });

  it("changes the theme from the toggle and remembers it", async () => {
    render(Sidebar);
    await fireEvent.click(
      screen.getByRole("radio", { name: "Dark" }).closest("button") ??
        screen.getByText("Dark"),
    );
    expect(document.documentElement.dataset["theme"]).toBe("dark");
    expect(localStorage.getItem("puddle.theme")).toBe("dark");
    await fireEvent.click(screen.getByText("System"));
    expect(document.documentElement.dataset["theme"]).toBeUndefined();
  });

  it("keeps the theme when the chosen one is clicked again", async () => {
    const set = vi.spyOn(theme, "set");
    const shown = toasts.items.length;
    render(Sidebar);
    await fireEvent.click(screen.getByText("System"));
    expect(set).not.toHaveBeenCalled();
    expect(theme.choice).toBe("system");
    expect(toasts.items).toHaveLength(shown);
  });

  it("says why a theme choice wasn't saved", async () => {
    vi.spyOn(theme, "set").mockResolvedValue({
      ok: false,
      message: "Settings file is damaged.",
    });
    render(Sidebar);
    await fireEvent.click(screen.getByText("Dark"));
    await vi.waitFor(() =>
      expect(toasts.items.map((t) => t.message)).toContain(
        "The theme changed here but wasn't saved. Settings file is damaged.",
      ),
    );
  });
});

describe("root layout", () => {
  it("renders the skip link, nav and page, applies the stored theme and starts the live state", () => {
    localStorage.setItem("puddle.theme", "light");
    const { unmount } = render(Layout, { children: (() => {}) as never });
    expect(
      screen.getByRole("link", { name: "Skip to content" }),
    ).toHaveAttribute("href", "#main");
    expect(screen.getByRole("main")).toHaveAttribute("id", "main");
    expect(document.documentElement.dataset["theme"]).toBe("light");
    expect(theme.choice).toBe("light");
    expect(live.start).toHaveBeenCalledTimes(1);
    expect(health.start).toHaveBeenCalledTimes(1);
    expect(live.subscribe).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("region", { name: "Notices" })).toBeInTheDocument();
    expect(document.title).toBe("Inbox - Puddle");
    unmount();
  });

  it("explains a refused or missing service in a status message", () => {
    live.problem = "unauthorized";
    const { unmount } = render(Layout, { children: (() => {}) as never });
    expect(screen.getByRole("status")).toHaveTextContent(/can't sign in/);
    unmount();
    live.problem = "unreachable";
    render(Layout, { children: (() => {}) as never });
    expect(screen.getByRole("status")).toHaveTextContent(/isn't answering/);
  });

  it("names an erroring service and a paused event stream", () => {
    live.problem = "failing";
    live.problemDetail = "Puddle's service hit an error (status 500).";
    const { unmount } = render(Layout, { children: (() => {}) as never });
    expect(screen.getByRole("status")).toHaveTextContent(
      "Puddle's service hit an error (status 500). Retrying.",
    );
    unmount();
    live.problem = null;
    live.streamError = "the event stream answered 500";
    render(Layout, { children: (() => {}) as never });
    expect(screen.getByRole("status")).toHaveTextContent(
      /Live updates paused: the event stream answered 500/,
    );
  });

  it("says a refused event stream stopped for good, with the service's reason and what to do", () => {
    live.stream = "refused";
    live.streamRefusal = {
      reason:
        "the service refused the event stream: requests from this origin are not allowed (status 403)",
      advice:
        "Open Puddle from its own app window or tray icon, not from another address.",
    };
    render(Layout, { children: (() => {}) as never });
    expect(screen.getByRole("status")).toHaveTextContent(
      "Live updates stopped: the service refused the event stream: requests from this origin are not allowed (status 403). Open Puddle from its own app window or tray icon, not from another address.",
    );
  });

  it("leaves the sidebar out of the first-run flow and names its step in the title", () => {
    url.pathname = "/welcome/check";
    render(Layout, { children: (() => {}) as never });
    expect(
      screen.queryByRole("navigation", { name: "Main" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("main")).toBeInTheDocument();
    expect(document.title).toBe("System check - Puddle");
  });

  it("puts the pending count in the document title", () => {
    live.pending = 2;
    render(Layout, { children: (() => {}) as never });
    expect(document.title).toBe("Inbox (2) - Puddle");
  });
});

describe("pages", () => {
  it("the app renders in the browser only", () => {
    expect({ ssr, prerender }).toEqual({ ssr: false, prerender: false });
  });

  it("the error page tells a 404 from another failure", () => {
    render(ErrorPage);
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(
      "Page not found",
    );
  });
});
