// SPDX-License-Identifier: AGPL-3.0-or-later
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
  const icept = await import("#lib/testing/fake-interception.ts");
  const interception = new icept.FakeInterception();
  const firstRun = new first.FakeFirstRun();
  const prefs = new settings.FakeSettings();
  const workspaces = new ws.FakeWorkspaces();
  const first_run_paths = ["/api/first-run", "/api/doctor"];
  // One client over the three fakes, by path, like the one API.
  const api = {
    GET: (path: string, init?: unknown) =>
      path === "/api/interception"
        ? interception.GET(path)
        : first_run_paths.includes(path)
          ? firstRun.GET(path)
          : path.startsWith("/api/workspaces")
            ? workspaces.GET(path, init as never)
            : prefs.GET(path),
    PUT: (path: string, init: { body: unknown }) =>
      path === "/api/interception"
        ? interception.PUT(path, init as never)
        : path === "/api/first-run"
          ? firstRun.PUT(path, init)
          : prefs.PUT(path, init as never),
    POST: (path: string, init: never) =>
      path === "/api/dev-certificate/trust"
        ? firstRun.POST(path)
        : workspaces.POST(path, init),
  };
  return { api, firstRun, interception, prefs, workspaces };
});
vi.mock("#lib/api/client.ts", () => ({ api: h.api }));

const network = vi.hoisted(() => ({
  report: null as unknown,
  status: "ready" as string,
}));
vi.mock("#lib/stores/network-health.svelte.ts", () => ({
  networkHealth: network,
}));

import { brokenReport, healthyReport } from "#lib/testing/fake-first-run.ts";
import { report } from "#lib/testing/fake-network.ts";
import { doctor } from "#lib/stores/doctor.svelte.ts";
import { firstRun } from "#lib/stores/first-run.svelte.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaces as workspaceStore } from "#lib/stores/workspaces.svelte.ts";
import { density } from "#lib/theme/density.svelte.ts";
import { palette } from "#lib/theme/palette.svelte.ts";
import { NO_CUSTOM } from "#lib/theme/palette.ts";
import { theme } from "#lib/theme/theme.svelte.ts";
import { around, isWelcome, STEPS } from "#lib/welcome/steps.ts";
import { closedDetailsAround } from "#lib/welcome/reachable.ts";
import walk from "#lib/welcome/walk.json";
import Layout from "./+layout.svelte";
import Welcome from "./+page.svelte";
import Certificates from "./certificates/+page.svelte";
import Check from "./check/+page.svelte";
import Connect from "./connect/+page.svelte";
import Interception from "./interception/+page.svelte";
import Look from "./look/+page.svelte";
import Done from "./done/+page.svelte";

beforeEach(() => {
  nav.path = "/welcome";
  nav.search = "";
  nav.goto.mockClear();
  h.firstRun.calls = [];
  h.firstRun.bodies = [];
  h.firstRun.down = false;
  h.firstRun.refusePut = false;
  h.firstRun.trustAnswer = {
    dev_certificate: "trusted",
    detail: null,
    waiting: false,
  };
  h.firstRun.holdTrust = null;
  h.firstRun.state = {
    completed: false,
    completed_at: null,
    step: null,
    dev_certificate: "not_checked",
  };
  h.prefs.reset();
  h.interception.reset();
  h.workspaces.list = [];
  // The create form checks a name against the store's list, which an earlier test filled.
  workspaceStore.list = [];
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
      "Done: Welcome",
      "Done: System check",
      "Done: Certificates",
      "4 Connect",
      "5 Interception",
      "6 Look",
      "7 Done",
    ]);
    expect(
      items.filter((li) => li.getAttribute("aria-current") === "step"),
    ).toEqual([items[3]]);
    expect(screen.getByText("Puddle")).toBeInTheDocument();
    // A finished step is a tick and normal text; the steps still to do keep their number.
    expect(items[0]!.querySelector("svg.tick")).not.toBeNull();
    expect(items[3]!.querySelector("svg")).toBeNull();
  });

  it("tells Puddle which step the person is on, so the flow resumes there", async () => {
    nav.path = "/welcome/interception";
    render(Layout, { children: (() => {}) as never });
    await waitFor(() => expect(h.firstRun.state.step).toBe("interception"));
    expect(h.firstRun.bodies).toEqual([{ step: "interception" }]);
  });

  it("records no step for the system check opened from Settings", async () => {
    nav.path = "/welcome/check";
    nav.search = "?from=settings";
    render(Layout, { children: (() => {}) as never });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(h.firstRun.bodies).toEqual([]);
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

  it("offers a small way to import a backup, apart from the steps", () => {
    render(Welcome);
    expect(
      screen.getByRole("link", { name: "Import a backup" }),
    ).toHaveAttribute("href", "/welcome/import");
  });

  it("skipping the setup records it and opens the workspace list", async () => {
    render(Welcome);
    await fireEvent.click(screen.getByRole("button", { name: "Skip setup" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(h.firstRun.bodies).toEqual([{ completed: true }]);
    expect(h.firstRun.state.completed).toBe(true);
    expect(toasts.items).toEqual([]);
  });

  it("skipping the setup takes the recommended interception mode when none was chosen", async () => {
    render(Welcome);
    await fireEvent.click(screen.getByRole("button", { name: "Skip setup" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(h.interception.view.mode).toBe("all_except_curated");
    expect(h.interception.view.chosen).toBe(true);
  });

  it("skipping the setup leaves a mode that was already chosen", async () => {
    h.interception.view = {
      ...h.interception.view,
      mode: "all",
      chosen: true,
      notice_seen: true,
    };
    render(Welcome);
    await fireEvent.click(screen.getByRole("button", { name: "Skip setup" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(h.interception.view.mode).toBe("all");
    expect(h.interception.bodies).toEqual([]);
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
    // Where the details are is named, not linked: a link would leave the flow.
    expect(screen.getByText(/Settings › Network health/)).toBeVisible();
    expect(screen.queryByRole("link", { name: /Network health/ })).toBeNull();
    expect(screen.getByRole("link", { name: "Back" })).toHaveAttribute(
      "href",
      "/welcome/check",
    );
    expect(screen.getByRole("link", { name: "Continue" })).toHaveAttribute(
      "href",
      "/welcome/connect",
    );
  });

  it("says what the step is for and keeps the long text behind More details", async () => {
    h.firstRun.state = { ...h.firstRun.state, dev_certificate: "needs_trust" };
    render(Certificates);
    expect(await screen.findByText(/Nothing else needs it/)).toBeVisible();
    expect(screen.queryByText(/Nothing to decide here/)).toBeNull();
    const more = screen.getByText("More details");
    expect(more.closest("details")).not.toHaveAttribute("open");
    await fireEvent.click(more);
    expect(more.closest("details")).toHaveAttribute("open");
    expect(screen.getByText(/Manage user certificates/)).toBeVisible();
    expect(screen.getByText(/If you skip, everything in Puddle/)).toBeVisible();
  });

  it("offers Trust it now while the certificate needs trust, and a yes ends the offer", async () => {
    h.firstRun.state = { ...h.firstRun.state, dev_certificate: "needs_trust" };
    render(Certificates);
    // The short text is on screen; Impact and Remediation open on a press.
    expect(
      await screen.findByText(/doesn't trust Puddle's localhost certificate/),
    ).toBeVisible();
    const remediation = screen.getByText("Remediation");
    expect(remediation.closest("details")).not.toHaveAttribute("open");
    await fireEvent.click(remediation);
    expect(remediation.closest("details")).toHaveAttribute("open");
    expect(
      screen.getByText(/Windows asks once, in a Security Warning/),
    ).toBeVisible();
    await fireEvent.click(screen.getByRole("button", { name: "Trust it now" }));
    expect(h.firstRun.calls).toContain("POST /api/dev-certificate/trust");
    const status = screen.getByRole("status");
    await waitFor(() =>
      expect(status).toHaveTextContent("Development certificate trusted."),
    );
    expect(
      screen.getByText("Development certificate trusted", {
        selector: "p.title",
      }),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Trust it now" }),
    ).not.toBeInTheDocument();
    // The button the keyboard was on is gone: the outcome has the focus.
    expect(status).toHaveFocus();
  });

  it("waits for Windows with the button off, and offers it again after a no", async () => {
    let answer = () => {};
    h.firstRun.holdTrust = new Promise((resolve) => (answer = resolve));
    h.firstRun.trustAnswer = {
      dev_certificate: "trust_declined",
      detail: null,
      waiting: false,
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
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(
        "Development certificate: not trusted.",
      ),
    );
    expect(screen.getByRole("button", { name: "Trust it now" })).toBeEnabled();
  });

  it("says Windows is still waiting when an earlier press has its dialog open", async () => {
    h.firstRun.trustAnswer = {
      dev_certificate: "needs_trust",
      detail: null,
      waiting: true,
    };
    h.firstRun.state = { ...h.firstRun.state, dev_certificate: "needs_trust" };
    render(Certificates);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Trust it now" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(
        /still waiting for your answer.*Pressing again does not open another/,
      ),
    );
    expect(screen.getByRole("button", { name: "Trust it now" })).toBeEnabled();
  });

  it("shows the system's words when trusting failed", async () => {
    h.firstRun.trustAnswer = {
      dev_certificate: "trust_failed",
      detail: "os error 5",
      waiting: false,
    };
    h.firstRun.state = {
      ...h.firstRun.state,
      dev_certificate: "trust_declined",
    };
    render(Certificates);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Trust it now" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(
        "Development certificate: couldn't be trusted. Details: os error 5",
      ),
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

describe("interception step", () => {
  const radio = (name: RegExp) => screen.getByRole("radio", { name });

  it("preselects the recommended mode on a first visit and saves nothing until Continue", async () => {
    render(Interception);
    await screen.findByRole("group", { name: "Interception" });
    expect(
      screen.getByRole("heading", {
        level: 1,
        name: "Which hosts should Puddle decrypt?",
      }),
    ).toHaveFocus();
    expect(radio(/except sites known to break/)).toBeChecked();
    expect(screen.getByText("recommended")).toBeInTheDocument();
    expect(h.interception.bodies).toEqual([]);
    expect(screen.getByRole("link", { name: "Back" })).toHaveAttribute(
      "href",
      "/welcome/connect",
    );
  });

  it("names the mechanism and keeps the long text behind More details", async () => {
    render(Interception);
    await screen.findByRole("group", { name: "Interception" });
    expect(
      screen.getByText(/reads \(MITMs\) HTTPS traffic to monitor it/),
    ).toBeVisible();
    expect(screen.getByText(/change this later in Settings/)).toBeVisible();
    const more = screen.getByText("More details");
    expect(more.closest("details")).not.toHaveAttribute("open");
    await fireEvent.click(more);
    expect(more.closest("details")).toHaveAttribute("open");
    expect(screen.getByText(/own certificate, which/)).toBeVisible();
    expect(screen.getByText(/credential injection/)).toBeVisible();
    expect(screen.getByText(/no credential is injected/)).toBeVisible();
  });

  it("saves the chosen mode and goes to the next step", async () => {
    render(Interception);
    await screen.findByRole("group", { name: "Interception" });
    await fireEvent.click(radio(/^Read all HTTPS traffic/));
    await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/welcome/look"));
    expect(h.interception.bodies).toEqual([{ mode: "all" }]);
  });

  it("shows the mode chosen before when the step is opened again", async () => {
    h.interception.view = {
      ...h.interception.view,
      mode: "known_only",
      chosen: true,
    };
    render(Interception);
    await screen.findByRole("group", { name: "Interception" });
    await waitFor(() =>
      expect(radio(/only where a rule needs it/)).toBeChecked(),
    );
  });

  it("says why the setting could not be read and offers no choice", async () => {
    h.interception.refuse = true;
    render(Interception);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Couldn't read this setting.",
    );
    expect(screen.queryByRole("radio")).toBeNull();
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
  });

  it("stays on the step and says why when the choice could not be saved", async () => {
    render(Interception);
    await screen.findByRole("group", { name: "Interception" });
    h.interception.refuse = true;
    await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(nav.goto).not.toHaveBeenCalled();
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
      "/welcome/done",
    );
    expect(screen.getByRole("link", { name: "Back" })).toHaveAttribute(
      "href",
      "/welcome/interception",
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

describe("done step", () => {
  it("says where settings live and that feedback is welcome, and goes back to the look step", () => {
    render(Done);
    expect(
      screen.getByRole("heading", { level: 1, name: "You're all set" }),
    ).toHaveFocus();
    expect(screen.getByText(/holds every choice you made here/)).toBeVisible();
    expect(screen.getByText(/Feedback is very welcome/)).toBeVisible();
    expect(screen.getByRole("link", { name: "Back" })).toHaveAttribute(
      "href",
      "/welcome/look",
    );
    expect(screen.getByRole("button", { name: "Open Puddle" })).toBeEnabled();
  });

  it("offers no workspace form and no tutorial", () => {
    render(Done);
    expect(screen.queryByRole("button", { name: /Create/ })).toBeNull();
    expect(screen.queryByText(/tutorial/i)).toBeNull();
  });

  it("opening Puddle ends the flow without creating a workspace", async () => {
    render(Done);
    await fireEvent.click(screen.getByRole("button", { name: "Open Puddle" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(h.firstRun.state.completed).toBe(true);
    expect(h.workspaces.calls).toEqual([]);
  });

  it("opens the list even when it could not keep that the flow is done, and says so", async () => {
    h.firstRun.refusePut = true;
    render(Done);
    await fireEvent.click(screen.getByRole("button", { name: "Open Puddle" }));
    await waitFor(() => expect(nav.goto).toHaveBeenCalledWith("/workspaces"));
    expect(toasts.items[0]?.message).toMatch(
      /newer Puddle.*may show again next time/,
    );
  });
});

describe("every step", () => {
  // Leaving the flow half done loses it: the steps after this one keep their defaults unseen, and
  // the flow only comes back, at its first step, the next time Puddle opens.
  it.each([
    ["welcome", Welcome],
    ["check", Check],
    ["certificates", Certificates],
    ["connect", Connect],
    ["interception", Interception],
    ["look", Look],
    ["done", Done],
  ] as const)(
    "the %s step links only to steps of the flow",
    async (_, Step) => {
      render(Step as never);
      // Let each step load what it shows, so links that appear with its data are counted too.
      await new Promise((resolve) => setTimeout(resolve, 0));
      const start = screen.queryByRole("button", {
        name: "Start system check",
      });
      if (start) {
        await fireEvent.click(start);
        await screen.findByText("No problems found.");
      }
      const links = screen.queryAllByRole("link");
      for (const link of links) {
        const href = link.getAttribute("href") ?? "";
        expect(
          isWelcome(new URL(href, "http://127.0.0.1").pathname),
          href,
        ).toBe(true);
      }
    },
  );
});

// walk.json is what the installed-app walk (crates/puddle-app/e2e/usable.mjs) and ui/e2e/first-run.spec.ts
// press and wait for. A page that stops matching it fails here, in the change that made it.
describe("the walk list", () => {
  const pages = {
    welcome: Welcome,
    check: Check,
    certificates: Certificates,
    connect: Connect,
    interception: Interception,
    look: Look,
    done: Done,
  } as const;

  it("names the steps of the flow, in order, with their paths", () => {
    expect(walk.steps.map((s) => [s.id, s.href])).toEqual(
      STEPS.map((s) => [s.id, s.href]),
    );
  });

  it.each(walk.steps.map((step) => [step.id, step] as const))(
    "the %s page has its heading and the controls the list names",
    async (id, step) => {
      nav.path = step.href;
      doctor.status = "idle";
      doctor.report = null;
      render(pages[id as keyof typeof pages] as never);
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(
        await screen.findByRole("heading", { level: 1, name: step.heading }),
      ).toBeInTheDocument();
      // A control the walk presses must be reachable: not inside a closed details.
      const control = (c: { role: string; name: string }) => {
        const found = screen.queryByRole(c.role as "link" | "button", {
          name: c.name,
        });
        if (found) {
          expect(
            closedDetailsAround(found),
            `${c.role} "${c.name}" sits in a closed details; list its summary in walk.json behindDetails and press it first`,
          ).toBeNull();
        }
        return found;
      };
      if ("start" in step && "afterStart" in step) {
        // Nothing runs, and nothing of its result shows, until Start is pressed.
        expect(control(step.afterStart)).toBeNull();
        await fireEvent.click(
          screen.getByRole("button", { name: step.start.name }),
        );
        await screen.findByText("No problems found.");
        expect(control(step.afterStart)).not.toBeNull();
      }
      if ("continue" in step) {
        const on = control(step.continue);
        expect(
          on,
          `${step.continue.role} "${step.continue.name}"`,
        ).not.toBeNull();
        if (step.continue.role === "link") {
          expect(on).toHaveAttribute("href", around(step.id as never).next);
        }
      }
    },
  );
});

describe("the blocked system check", () => {
  beforeEach(() => {
    // The check's result outlives a step, as when the person comes back to it.
    doctor.status = "idle";
    doctor.report = null;
  });
  afterEach(() => {
    h.firstRun.report = healthyReport();
  });

  it("offers one way out of the flow, named as such", async () => {
    h.firstRun.report = brokenReport();
    render(Check);
    await fireEvent.click(
      screen.getByRole("button", { name: "Start system check" }),
    );
    await screen.findByText(/1 problem to fix/);
    // Puddle can't run workspaces here, so the person may leave setup on purpose; it comes back.
    const out = screen
      .getAllByRole("link")
      .filter(
        (link) =>
          !isWelcome(
            new URL(link.getAttribute("href") ?? "", "http://127.0.0.1")
              .pathname,
          ),
      )
      .map((link) => link.textContent?.trim());
    expect(out).toEqual(["Leave setup for now"]);
  });
});
