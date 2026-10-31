// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const nav = vi.hoisted(() => ({
  path: "/welcome",
  search: "",
  goto: vi.fn(async (_to: string) => undefined),
}));
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(`http://127.0.0.1${nav.path}${nav.search}`);
    },
  },
}));
vi.mock("$app/navigation", () => ({ goto: nav.goto }));

const h = await vi.hoisted(async () => {
  const first = await import("#lib/testing/fake-first-run.ts");
  const settings = await import("#lib/testing/fake-settings.ts");
  const ws = await import("#lib/testing/fake-workspaces.ts");
  const firstRun = new first.FakeFirstRun();
  const prefs = new settings.FakeSettings();
  const workspaces = new ws.FakeWorkspaces();
  const first_run_paths = ["/api/first-run", "/api/doctor"];
  // One client over the three fakes, by path, like the one API.
  const api = {
    GET: (path: string, init?: unknown) =>
      first_run_paths.includes(path)
        ? firstRun.GET(path)
        : path.startsWith("/api/workspaces")
          ? workspaces.GET(path, init as never)
          : prefs.GET(path),
    PUT: (path: string, init: { body: unknown }) =>
      path === "/api/first-run"
        ? firstRun.PUT(path, init)
        : prefs.PUT(path, init as never),
    POST: (path: string, init: never) =>
      path === "/api/dev-certificate/trust"
        ? firstRun.POST(path)
        : workspaces.POST(path, init),
  };
  return { api, firstRun, prefs, workspaces };
});
vi.mock("#lib/api/client.ts", () => ({ api: h.api }));

const network = vi.hoisted(() => ({
  report: null as unknown,
  status: "ready" as string,
}));
vi.mock("#lib/stores/network-health.svelte.ts", () => ({
  networkHealth: network,
}));

import { report } from "#lib/testing/fake-network.ts";
import { firstRun } from "#lib/stores/first-run.svelte.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { density } from "#lib/theme/density.svelte.ts";
import { palette } from "#lib/theme/palette.svelte.ts";
import { NO_CUSTOM } from "#lib/theme/palette.ts";
import { theme } from "#lib/theme/theme.svelte.ts";
import Layout from "./+layout.svelte";
import Welcome from "./+page.svelte";
import Certificates from "./certificates/+page.svelte";
import Look from "./look/+page.svelte";
import Workspace from "./workspace/+page.svelte";

beforeEach(() => {
  nav.path = "/welcome";
  nav.search = "";
  nav.goto.mockClear();
  h.firstRun.calls = [];
  h.firstRun.bodies = [];
  h.firstRun.down = false;
  h.firstRun.refusePut = false;
  h.firstRun.trustAnswer = { dev_certificate: "trusted", detail: null };
  h.firstRun.holdTrust = null;
  h.firstRun.state = {
    completed: false,
    completed_at: null,
    dev_certificate: "not_checked",
  };
  h.prefs.reset();
  h.workspaces.list = [];
  h.workspaces.calls = [];
  h.workspaces.bodies = [];
  h.workspaces.refuse.clear();
  firstRun.state = null;
  firstRun.status = "loading";
  globalSettings.view = null;
  globalSettings.consents = null;
  globalSettings.status = "loading";
  network.report = report();
  network.status = "ready";
  toasts.items = [];
  localStorage.clear();
  delete document.documentElement.dataset["theme"];
  delete document.documentElement.dataset["density"];
  theme.choice = "system";
  density.choice = "comfortable";
  delete document.documentElement.dataset["palette"];
  document.documentElement.removeAttribute("style");
  palette.choice = "pond";
  palette.custom = { ...NO_CUSTOM };
});
afterEach(cleanup);

describe("the flow's frame", () => {
  it("lists the steps in order and marks the current one and the ones done", () => {
    nav.path = "/welcome/connect";
    render(Layout, { children: (() => {}) as never });
    const steps = screen.getByRole("list", { name: "Setup steps" });
    const items = within(steps).getAllByRole("listitem");
    expect(
      items.map((li) => li.textContent?.replace(/\s+/g, " ").trim()),
    ).toEqual([
      "1 Welcome, done",
      "2 System check, done",
      "3 Certificates, done",
      "4 Connect",
      "5 Look",
      "6 First workspace",
    ]);
    expect(
      items.filter((li) => li.getAttribute("aria-current") === "step"),
    ).toEqual([items[3]]);
    expect(screen.getByText("Puddle")).toBeInTheDocument();
  });

  it("leaves the step bar out when the system check runs on its own from Settings", () => {
    nav.path = "/welcome/check";
    nav.search = "?from=settings";
    render(Layout, { children: (() => {}) as never });
    expect(screen.queryByRole("list", { name: "Setup steps" })).toBeNull();
  });
});

describe("welcome step", () => {
  it("greets, and goes on to the system check", () => {
    render(Welcome);
    expect(
      screen.getByRole("heading", { level: 1, name: "Welcome to Puddle" }),
    ).toHaveFocus();
    expect(screen.getByRole("link", { name: "Get started" })).toHaveAttribute(
      "href",
      "/welcome/check",
    );
  });

  it("skipping the setup records it and opens the workspace list", async () => {
    render(Welcome);
    await fireEvent.click(screen.getByRole("button", { name: "Skip setup" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(h.firstRun.bodies).toEqual([{ completed: true }]);
    expect(h.firstRun.state.completed).toBe(true);
    expect(toasts.items).toEqual([]);
  });

  it("opens the workspace list even when it could not keep that, and says so", async () => {
    h.firstRun.refusePut = true;
    render(Welcome);
    await fireEvent.click(screen.getByRole("button", { name: "Skip setup" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(toasts.items[0]?.message).toMatch(
      /newer Puddle.*may show again next time/,
    );
    expect(toasts.items[0]?.tone).toBe("error");
  });
});

describe("certificates step", () => {
  it("says the development certificate was not checked and counts the company roots", async () => {
    render(Certificates);
    const list = screen.getByRole("list", { name: "Certificates" });
    await waitFor(() =>
      expect(h.firstRun.calls).toContain("GET /api/first-run"),
    );
    const items = within(list).getAllByRole("listitem");
    expect(items[0]).toHaveTextContent(
      "Development certificate: not checked yet",
    );
    expect(items[1]).toHaveTextContent("1 company root certificate found");
    expect(
      screen.getByRole("link", { name: /Network health/ }),
    ).toHaveAttribute("href", "/settings/network-health");
    expect(screen.getByRole("link", { name: "Back" })).toHaveAttribute(
      "href",
      "/welcome/check",
    );
    expect(screen.getByRole("link", { name: "Continue" })).toHaveAttribute(
      "href",
      "/welcome/connect",
    );
  });

  it("says once that Puddle uses an existing .NET development certificate", async () => {
    h.firstRun.state = {
      ...h.firstRun.state,
      dev_certificate: "reusing_existing",
    };
    render(Certificates);
    expect(
      await screen.findByText(
        "Using your existing .NET development certificate",
      ),
    ).toBeVisible();
    expect(screen.getByText(/No trust dialog needed/)).toBeVisible();
  });

  it("offers Trust it now while the certificate needs trust, and a yes ends the offer", async () => {
    h.firstRun.state = { ...h.firstRun.state, dev_certificate: "needs_trust" };
    render(Certificates);
    expect(
      await screen.findByText(
        /one Security Warning that names the certificate/,
      ),
    ).toBeVisible();
    await fireEvent.click(screen.getByRole("button", { name: "Trust it now" }));
    expect(h.firstRun.calls).toContain("POST /api/dev-certificate/trust");
    expect(
      await screen.findByText("Development certificate trusted"),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Trust it now" }),
    ).not.toBeInTheDocument();
  });

  it("waits for Windows with the button off, and offers it again after a no", async () => {
    let answer = () => {};
    h.firstRun.holdTrust = new Promise((resolve) => (answer = resolve));
    h.firstRun.trustAnswer = {
      dev_certificate: "trust_declined",
      detail: null,
    };
    h.firstRun.state = { ...h.firstRun.state, dev_certificate: "needs_trust" };
    render(Certificates);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Trust it now" }),
    );
    const waiting = await screen.findByRole("button", {
      name: "Waiting for Windows…",
    });
    expect(waiting).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent(/Security Warning/);
    answer();
    expect(
      await screen.findByText("Development certificate: not trusted"),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Trust it now" })).toBeEnabled();
    expect(screen.getByRole("status")).toHaveTextContent(/^$/);
  });

  it("shows the system's words when trusting failed or the service did not answer", async () => {
    h.firstRun.trustAnswer = {
      dev_certificate: "trust_failed",
      detail: "os error 5",
    };
    h.firstRun.state = {
      ...h.firstRun.state,
      dev_certificate: "trust_declined",
    };
    render(Certificates);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Trust it now" }),
    );
    expect(await screen.findByRole("status")).toHaveTextContent(
      "Details: os error 5",
    );
    expect(
      screen.queryByRole("button", { name: "Trust it now" }),
    ).not.toBeInTheDocument();
  });

  it("has no Trust button where Windows cannot be asked", async () => {
    h.firstRun.state = { ...h.firstRun.state, dev_certificate: "unsupported" };
    render(Certificates);
    await screen.findByText(
      "Development certificate: not trusted on this system yet",
    );
    expect(
      screen.queryByRole("button", { name: "Trust it now" }),
    ).not.toBeInTheDocument();
  });

  it("says when the company roots could not be read", () => {
    network.report = null;
    network.status = "failed";
    render(Certificates);
    expect(screen.getByText("Company certificates: not read")).toBeVisible();
    cleanup();
    network.status = "loading";
    render(Certificates);
    expect(screen.getByText("Company certificates")).toBeVisible();
    cleanup();
    network.status = "unavailable";
    render(Certificates);
    expect(screen.getByText("Company certificates: not read")).toBeVisible();
  });
});

describe("look step", () => {
  const radio = (name: string) =>
    screen.getByRole("radio", { name: new RegExp(`^${name}`) });

  it("offers the theme and the density with the stored ones selected", () => {
    render(Look);
    expect(
      screen.getByRole("heading", { level: 1, name: "Look" }),
    ).toHaveFocus();
    expect(radio("Follow my system")).toBeChecked();
    expect(radio("Comfortable")).toBeChecked();
    expect(screen.getByRole("link", { name: "Continue" })).toHaveAttribute(
      "href",
      "/welcome/workspace",
    );
    expect(screen.getByRole("link", { name: "Back" })).toHaveAttribute(
      "href",
      "/welcome/connect",
    );
  });

  it("applies a theme at once and keeps it in the settings", async () => {
    render(Look);
    await fireEvent.click(radio("Dark"));
    expect(await screen.findByText("Theme saved.")).toBeVisible();
    expect(document.documentElement.dataset["theme"]).toBe("dark");
    expect(h.prefs.ui.theme).toBe("dark");
    expect(radio("Dark")).toBeChecked();
  });

  it("applies a density at once and keeps it in the settings", async () => {
    render(Look);
    await fireEvent.click(radio("Compact"));
    expect(await screen.findByText("Density saved.")).toBeVisible();
    expect(document.documentElement.dataset["density"]).toBe("compact");
    expect(h.prefs.ui.density).toBe("compact");
  });

  it("offers the preset palettes, Pond first and selected, and no custom one", () => {
    render(Look);
    const names = screen
      .getAllByRole("radio")
      .filter((r) => r.getAttribute("name") === "palette")
      .map((r) => r.closest("label")?.querySelector("b")?.textContent);
    expect(names).toEqual(["Pond", "Dusk", "Meadow", "Ember", "Graphite"]);
    expect(radio("Pond")).toBeChecked();
  });

  it("applies a palette at once and keeps it in the settings", async () => {
    render(Look);
    await fireEvent.click(radio("Meadow"));
    expect(await screen.findByText("Colours saved.")).toBeVisible();
    expect(document.documentElement.dataset["palette"]).toBe("meadow");
    expect(h.prefs.ui.palette).toBe("meadow");
    expect(radio("Meadow")).toBeChecked();
  });

  it("says when a palette applied here could not be kept", async () => {
    render(Look);
    h.prefs.refuse.set("PUT /api/settings", {
      status: 422,
      error: "invalid",
      message: "no",
    });
    await fireEvent.click(radio("Ember"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The colours changed here but Puddle couldn't save them. No.",
    );
  });

  it("says when a choice applied here could not be kept", async () => {
    render(Look);
    h.prefs.refuse.set("PUT /api/settings", {
      status: 422,
      error: "invalid",
      message: "no",
    });
    await fireEvent.click(radio("Light"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The theme changed here but Puddle couldn't save it. No.",
    );
    h.prefs.refuse.set("PUT /api/settings", {
      status: 422,
      error: "invalid",
      message: "no",
    });
    await fireEvent.click(radio("Compact"));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "The density changed here but Puddle couldn't save it.",
      ),
    );
  });
});

describe("first workspace step", () => {
  it("offers to create one or skip, and goes back to the look step", () => {
    render(Workspace);
    expect(
      screen.getByRole("heading", { level: 1, name: "Your first workspace" }),
    ).toHaveFocus();
    expect(screen.getByRole("link", { name: "Back" })).toHaveAttribute(
      "href",
      "/welcome/look",
    );
    expect(screen.getByRole("button", { name: "Skip" })).toBeEnabled();
    expect(
      screen.getByRole("button", { name: "Create a workspace" }),
    ).toBeEnabled();
  });

  it("skipping ends the flow without a workspace", async () => {
    render(Workspace);
    await fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(h.firstRun.state.completed).toBe(true);
    expect(h.workspaces.calls).toEqual([]);
  });

  it("creates the workspace in the usual form, then ends the flow", async () => {
    render(Workspace);
    await fireEvent.click(
      screen.getByRole("button", { name: "Create a workspace" }),
    );
    const dialog = await screen.findByRole("dialog", { name: "New workspace" });
    await fireEvent.input(within(dialog).getByLabelText(/Git repository/), {
      target: { value: "https://github.com/acme/shop-api.git" },
    });
    expect(within(dialog).getByLabelText("Name")).toHaveValue("shop-api");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Create workspace" }),
    );
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(h.workspaces.bodies[0]).toMatchObject({
      name: "shop-api",
      repositories: [{ url: "https://github.com/acme/shop-api.git" }],
    });
    expect(h.firstRun.state.completed).toBe(true);
  });

  it("keeps the flow open when the workspace is refused, with the reason in the form", async () => {
    h.workspaces.refuse.set("POST /api/workspaces", {
      status: 422,
      message: "SSH addresses aren't supported yet",
    });
    render(Workspace);
    await fireEvent.click(
      screen.getByRole("button", { name: "Create a workspace" }),
    );
    const dialog = await screen.findByRole("dialog");
    await fireEvent.input(within(dialog).getByLabelText(/Git repository/), {
      target: { value: "https://github.com/acme/shop-api.git" },
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Create workspace" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "SSH addresses aren't supported yet",
    );
    expect(nav.goto).not.toHaveBeenCalled();
    expect(h.firstRun.state.completed).toBe(false);
  });

  it("opens the list even when it could not keep that the flow is done, and says so", async () => {
    h.firstRun.refusePut = true;
    render(Workspace);
    await fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(toasts.items[0]?.message).toMatch(
      /newer Puddle.*may show again next time/,
    );
  });
});
