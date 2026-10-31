// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const url = vi.hoisted(() => ({ pathname: "/workspaces/demo", id: "demo" }));
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(`http://127.0.0.1${url.pathname}`);
    },
    get params() {
      return { id: url.id };
    },
  },
}));
const goto = vi.hoisted(() => vi.fn());
vi.mock("$app/navigation", () => ({ goto }));

const h = await vi.hoisted(async () => {
  const inbox = await import("#lib/testing/fake-inbox.ts");
  const ws = await import("#lib/testing/fake-workspaces.ts");
  const rules = await import("#lib/testing/fake-rules.ts");
  const requestRules = await import("#lib/testing/fake-request-rules.ts");
  return {
    inbox: new inbox.FakeInbox(),
    source: new inbox.FakeSource(),
    request: inbox.request,
    api: new ws.FakeWorkspaces(),
    workspace: ws.workspace,
    rules: new rules.FakeRules(),
    rule: rules.rule,
    requestRules: new requestRules.FakeRequestRules(),
    requestRule: requestRules.requestRule,
  };
});

vi.mock("#lib/stores/pending.svelte.ts", async (original) => {
  const mod = await original<typeof import("#lib/stores/pending.svelte.ts")>();
  return {
    ...mod,
    pending: new mod.PendingStore({
      api: h.inbox as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
});
vi.mock("#lib/stores/workspaces.svelte.ts", async (original) => {
  const mod =
    await original<typeof import("#lib/stores/workspaces.svelte.ts")>();
  return {
    ...mod,
    workspaces: new mod.WorkspaceStore({
      api: h.api as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
});
vi.mock("#lib/stores/rules.svelte.ts", async (original) => {
  const mod = await original<typeof import("#lib/stores/rules.svelte.ts")>();
  return {
    ...mod,
    rulesStore: new mod.RulesStore({
      api: h.rules as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
});
vi.mock("#lib/stores/request-rules.svelte.ts", async (original) => {
  const mod =
    await original<typeof import("#lib/stores/request-rules.svelte.ts")>();
  return {
    ...mod,
    requestRulesStore: new mod.RequestRulesStore({
      api: h.requestRules as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
});
vi.mock("#lib/api/client.ts", () => ({ api: h.api }));

import { notices } from "#lib/notify/notices.svelte.ts";
import { editorProblems } from "#lib/stores/editor-problems.svelte.ts";
import { pending } from "#lib/stores/pending.svelte.ts";
import { requestRulesStore } from "#lib/stores/request-rules.svelte.ts";
import { rulesStore } from "#lib/stores/rules.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaceActions } from "#lib/stores/workspace-actions.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import {
  checkout,
  cleanCheck,
  dirtyCheck,
  listing,
} from "#lib/testing/fake-workspaces.ts";
import DetailLayout from "./+layout.svelte";
import Overview from "./+page.svelte";
import Network from "./network/+page.svelte";
import Settings from "./settings/+page.svelte";

const {
  inbox,
  api,
  workspace,
  request,
  rules,
  rule,
  requestRules,
  requestRule,
} = h;

/** The overrides a workspace has now (all inherited, for one never changed). */
async function settingsOf(name: string) {
  const { data } = (await api.GET("/api/settings/workspaces/{workspace}", {
    params: { path: { workspace: name } },
  })) as unknown as { data: { overrides: Record<string, unknown> } };
  return data.overrides as never as (typeof api.overrides)[string];
}

function reset() {
  url.pathname = "/workspaces/demo";
  url.id = "demo";
  goto.mockReset();
  inbox.open = [];
  inbox.rules = [];
  inbox.calls = [];
  inbox.toggles = {};
  inbox.down = false;
  rules.down = false;
  api.list = [workspace("demo", { status: "running" })];
  api.checkouts = {};
  api.problems = [];
  editorProblems.problems = [];
  api.calls = [];
  api.bodies = [];
  api.down = false;
  api.check = null;
  api.overrides = {};
  api.unknown = {};
  api.kept = {};
  api.interception = {};
  api.globalNeverDecrypt = [];
  api.learned = {};
  api.refuse.clear();
  rules.rules = [];
  rules.calls = [];
  pending.rows = [];
  pending.status = "loading";
  rulesStore.rules = [];
  rulesStore.status = "loading";
  requestRules.rules = [];
  requestRules.calls = [];
  requestRulesStore.rules = [];
  requestRulesStore.status = "loading";
  workspaces.list = [];
  workspaces.status = "loading";
  workspaces.progress = {};
  workspaces.oom = {};
  workspaceActions.createOpen = false;
  workspaceActions.connectOpen = false;
  workspaceActions.trustOpen = false;
  workspaceActions.deleteOpen = false;
  workspaceActions.deleting = null;
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
}

beforeEach(reset);
afterEach(cleanup);

async function load() {
  await Promise.all([
    workspaces.refresh(),
    pending.refresh(),
    rulesStore.refresh(),
    requestRulesStore.refresh(),
  ]);
}

describe("the detail layout", () => {
  it("shows the name, the state, the actions and the tabs with the current one marked", async () => {
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    expect(
      await screen.findByRole("heading", { level: 1, name: "demo" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Running")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Stop demo" }),
    ).toBeInTheDocument();
    const tabs = within(
      screen.getByRole("navigation", { name: "Workspace sections" }),
    );
    expect(tabs.getAllByRole("link").map((l) => l.textContent?.trim())).toEqual(
      [
        "Overview",
        "Network",
        "Git",
        "Environment",
        "Shell init",
        "Ports",
        "Settings",
      ],
    );
    expect(tabs.getByRole("link", { name: "Overview" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(tabs.getByRole("link", { name: "Settings" })).toHaveAttribute(
      "href",
      "/workspaces/demo/settings",
    );
  });

  it("titles the page with the name the user gave, keeps the identifier in the paths, and opens the rename dialog", async () => {
    api.list = [
      workspace("demo", { status: "running", display_name: "Web shop" }),
    ];
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    expect(
      await screen.findByRole("heading", { level: 1, name: "Web shop" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Stop Web shop" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute(
      "href",
      "/workspaces/demo/settings",
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Rename Web shop" }),
    );
    expect(workspaceActions.renameOpen).toBe(true);
    expect(workspaceActions.renaming?.id).toBe("demo");
  });

  it("renames a running workspace but not one that is being created or deleted", async () => {
    await load();
    const view = render(DetailLayout, { children: (() => {}) as never });
    expect(
      await screen.findByRole("button", { name: "Rename demo" }),
    ).toBeEnabled();
    view.unmount();
    for (const busy of ["creating", "deleting"] as const) {
      api.list = [workspace("demo", { status: "created", busy })];
      await load();
      const again = render(DetailLayout, { children: (() => {}) as never });
      expect(
        await screen.findByRole("button", { name: "Rename demo" }),
      ).toBeDisabled();
      again.unmount();
    }
    // Starting is no obstacle: only the label changes.
    api.list = [workspace("demo", { status: "starting", busy: "starting" })];
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    expect(
      await screen.findByRole("button", { name: "Rename demo" }),
    ).toBeEnabled();
  });

  it("says when the workspace's settings cannot be read, and says nothing otherwise", async () => {
    await load();
    const view = render(DetailLayout, { children: (() => {}) as never });
    await screen.findByRole("heading", { level: 1, name: "demo" });
    expect(screen.queryByRole("alert")).toBeNull();
    view.unmount();

    api.list = [
      workspace("demo", {
        status: "running",
        settings_error:
          "the settings cannot be read (global settings must be an object); direct SSH counts as off until they can",
      }),
    ];
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("can't read this workspace's settings");
    expect(alert).toHaveTextContent("must be an object");
    expect(
      within(alert).getByRole("link", { name: "Settings" }),
    ).toHaveAttribute("href", "/settings");
  });

  it("marks another tab, and counts what waits on the network tab", async () => {
    url.pathname = "/workspaces/demo/network";
    inbox.add(request(1, { workspace: "demo" as never }), "example.com");
    inbox.add(request(2, { workspace: "other" as never }), "example.com");
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    const link = await screen.findByRole("link", { name: /Network/ });
    expect(link).toHaveAttribute("aria-current", "page");
    expect(link).toHaveTextContent("1 waiting");
  });

  it("shows the progress line while something runs and says Start for a stopped workspace", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    await fireEvent.click(
      await screen.findByRole("button", { name: "Start demo" }),
    );
    await vi.waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent("Starting"),
    );
  });

  it("says which clone of how many is running, from the workspace's repositories", async () => {
    api.list = [
      workspace("demo", {
        status: "stopped",
        repositories: [
          { url: "https://github.com/acme/web.git", directory: "web" },
          { url: "https://github.com/acme/api.git", directory: "api" },
        ],
      }),
    ];
    await load();
    workspaces.progress = {
      demo: {
        step: "cloning",
        detail: null,
        repository: "api",
        operation: "creating",
        failed: false,
      },
    };
    render(DetailLayout, { children: (() => {}) as never });
    expect(await screen.findByRole("status")).toHaveTextContent(
      "Cloning api, 2 of 2",
    );
  });

  it("says when there is no such workspace", async () => {
    url.id = "nope";
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    expect(
      await screen.findByRole("heading", { name: "No such workspace" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Back to the workspaces" }),
    ).toHaveAttribute("href", "/workspaces");
    expect(goto).not.toHaveBeenCalled();
  });

  it("shows loading and failure lines before the workspace is known", async () => {
    render(DetailLayout, { children: (() => {}) as never });
    expect(screen.getByText(/Loading workspace/)).toBeInTheDocument();
    api.down = true;
    await workspaces.refresh();
    expect(
      await screen.findByText(/Couldn't read the workspace/),
    ).toBeInTheDocument();
  });

  it("goes back to the list when the workspace it showed is gone", async () => {
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    await screen.findByRole("heading", { level: 1, name: "demo" });
    api.list = [];
    await workspaces.refresh();
    await vi.waitFor(() => expect(goto).toHaveBeenCalledWith("/workspaces"));
  });
});

describe("the trusted mark", () => {
  it("shows on the workspace page only while direct SSH is on", async () => {
    api.list = [workspace("demo", { status: "running", direct_ssh: true })];
    await workspaces.refresh();
    const { unmount } = render(DetailLayout, { children: (() => {}) as never });
    expect(await screen.findByText("Trusted")).toBeInTheDocument();
    unmount();
    api.list = [workspace("demo", { status: "running" })];
    await workspaces.refresh();
    render(DetailLayout, { children: (() => {}) as never });
    await screen.findByRole("heading", { name: "demo" });
    expect(screen.queryByText("Trusted")).toBeNull();
  });
});

describe("the overview", () => {
  it("shows state, image, repository, resources and the network summary", async () => {
    rules.rules = [
      rule(1, { scope: { type: "workspace", workspace: "demo" } }),
      rule(2),
      rule(3, { expires_at: 1 }),
    ];
    inbox.add(request(1, { workspace: "demo" as never }), "example.com");
    const entry = {
      url: "https://github.com/acme/demo.git",
      directory: "demo",
    };
    api.checkouts["demo"] = listing([checkout("demo", { entry })]);
    await load();
    render(Overview);
    expect(await screen.findByText("ws-demo")).toBeInTheDocument();
    expect(
      await screen.findByRole("list", { name: "Checkouts of demo" }),
    ).toHaveTextContent("https://github.com/acme/demo.git");
    expect(screen.getByText("2 GiB of 32 GiB used")).toBeInTheDocument();
    expect(screen.getByText("8 GiB")).toBeInTheDocument();
    await vi.waitFor(() =>
      expect(
        screen.getByText(
          "1 waiting · 1 workspace rule · 1 rule for every workspace",
        ),
      ).toBeInTheDocument(),
    );
    await vi.waitFor(() =>
      expect(screen.getByText("global default")).toBeInTheDocument(),
    );
  });

  it("says where the memory comes from, and copes with settings that can't be read", async () => {
    await workspaces.refresh();
    api.overrides["demo"] = {
      ...(await settingsOf("demo")),
      memory: 4096,
    };
    await load();
    const view = render(Overview);
    await vi.waitFor(() =>
      expect(screen.getByText("workspace override")).toBeInTheDocument(),
    );
    view.unmount();
    api.refuse.set("GET /api/settings/workspaces/{workspace}", {
      status: 500,
      message: "x",
    });
    render(Overview);
    await screen.findByText("ws-demo");
    expect(screen.queryByText("workspace override")).toBeNull();
  });

  it("shows an out-of-memory notice that can be dismissed", async () => {
    await load();
    render(Overview);
    await screen.findByText("ws-demo");
    expect(screen.queryByRole("alert")).toBeNull();
    workspaces.handleEvent({
      type: "oom_kill",
      workspace: "demo",
      pid: 77,
      process: "node",
    });
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Out of memory");
    expect(alert).toHaveTextContent("node");
    expect(alert).toHaveTextContent("process 77");
    expect(
      within(alert).getByRole("link", { name: "Change memory" }),
    ).toHaveAttribute("href", "/workspaces/demo/settings");
    await fireEvent.click(
      within(alert).getByRole("button", { name: "Dismiss" }),
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("reclaims space on a running workspace, and not while it is changing", async () => {
    await load();
    render(Overview);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Reclaim space" }),
    );
    await vi.waitFor(() =>
      expect(api.calls).toContain("POST /api/workspaces/{id}/reclaim"),
    );
    await vi.waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Reclaim space" }),
      ).toBeDisabled(),
    );
  });

  it("shows the identifier only once the workspace has another name", async () => {
    await load();
    const view = render(Overview);
    await screen.findByText("Status");
    expect(screen.queryByText("Identifier")).toBeNull();
    view.unmount();
    api.list = [
      workspace("demo", { status: "running", display_name: "Web shop" }),
    ];
    await load();
    render(Overview);
    expect(await screen.findByText("Identifier")).toBeInTheDocument();
    expect(screen.getByText("Identifier").nextElementSibling).toHaveTextContent(
      "demo",
    );
    // The disk is still named after the identifier.
    expect(screen.getByText("ws-demo")).toBeInTheDocument();
  });

  it("can't delete a running workspace and says why", async () => {
    await load();
    render(Overview);
    const del = await screen.findByRole("button", {
      name: "Delete workspace…",
    });
    expect(del).toBeDisabled();
    expect(
      screen.getByText("Stop the workspace before deleting it."),
    ).toBeInTheDocument();
  });

  it("checks what a delete would lose, then opens the dialog with it", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    api.check = dirtyCheck("demo");
    await load();
    render(Overview);
    const del = await screen.findByRole("button", {
      name: "Delete workspace…",
    });
    expect(del).toBeEnabled();
    expect(
      screen.queryByText("Stop the workspace before deleting it."),
    ).toBeNull();
    await fireEvent.click(del);
    await vi.waitFor(() => expect(workspaceActions.deleteOpen).toBe(true));
    expect(workspaceActions.deleting?.check.fingerprint).toBe("fp-dirty");
  });

  it("says on the workspace's page why its browser editor could not be installed", async () => {
    api.problems = [
      {
        key: "editor-install:demo",
        workspace: "demo",
        title:
          "The browser editor could not be installed in demo: the download does not match",
        detail: "Terminals and desktop VS Code are not affected.",
      },
      {
        key: "editor-install:other",
        workspace: "other",
        title: "The browser editor could not be installed in other: no",
        detail: "x",
      },
    ];
    await load();
    render(Overview);
    expect(
      await screen.findByText(/installed in demo: the download does not match/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Terminals and desktop VS Code are not affected/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/installed in other/)).toBeNull();
  });

  it("shows no install note on a workspace whose editor works", async () => {
    await load();
    render(Overview);
    await screen.findByText("Status");
    expect(screen.queryByText(/could not be installed/)).toBeNull();
  });

  it("says a workspace with no volume can be restored or deleted, and that deleting loses nothing", async () => {
    api.list = [workspace("demo", { status: "volume_missing" })];
    api.check = cleanCheck("demo", { volume_missing: true });
    await load();
    render(Overview);
    expect(await screen.findByText(/Its disk is gone/)).toBeInTheDocument();
    expect(
      screen.getByText(/delete the workspace below; nothing is lost/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/deleting only removes the workspace from Puddle/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Delete workspace…" }),
    ).toBeEnabled();
  });

  it("shows why the settings can't be read when the API says, from either read", async () => {
    const reason =
      "Puddle cannot read the settings of workspace demo: settings must be an object";
    for (const path of [
      "/api/settings/workspaces/{workspace}",
      "/api/settings",
    ]) {
      await load();
      api.refuse.set(`GET ${path}`, {
        status: 409,
        error: "settings_unreadable",
        message: reason,
      });
      render(Settings);
      const alert = await screen.findByRole("alert");
      expect(alert).toHaveTextContent(
        "Couldn't read this workspace's settings.",
      );
      expect(alert).toHaveTextContent(reason);
      cleanup();
    }
  });

  it("shows nothing for a workspace it doesn't know", async () => {
    url.id = "nope";
    await load();
    const { container } = render(Overview);
    expect(container.textContent?.trim()).toBe("");
    api.check = cleanCheck("x");
  });
});

describe("the network tab", () => {
  beforeEach(() => {
    url.pathname = "/workspaces/demo/network";
  });

  it("lists this workspace's waiting requests, newest first, and not another's", async () => {
    inbox.add(
      request(1, {
        workspace: "demo" as never,
        host: "old.example.com",
        first_seen: 1,
      }),
      "example.com",
    );
    inbox.add(
      request(2, {
        workspace: "demo" as never,
        host: "new.example.com",
        first_seen: 9,
      }),
      "example.com",
    );
    inbox.add(
      request(3, { workspace: "other" as never, host: "theirs.example.org" }),
      "example.org",
    );
    await load();
    render(Network);
    await screen.findByText("new.example.com");
    const hosts = [...document.querySelectorAll("li.req .host")].map(
      (e) => e.textContent,
    );
    expect(hosts).toEqual(["new.example.com", "old.example.com"]);
    expect(screen.queryByText("theirs.example.org")).toBeNull();
    expect(screen.getByRole("heading", { name: /Waiting/ })).toHaveTextContent(
      "2",
    );
  });

  it("says when nothing waits, and when requests are still loading", async () => {
    await workspaces.refresh();
    render(Network);
    expect(await screen.findByText(/Loading requests/)).toBeInTheDocument();
    await load();
    expect(
      await screen.findByText("Nothing is waiting for demo."),
    ).toBeInTheDocument();
  });

  it("says a read failed instead of showing an empty list, and warns when a refresh failed", async () => {
    await load();
    render(Network);
    await screen.findByText("Nothing is waiting for demo.");
    inbox.down = true;
    rules.down = true;
    await pending.refresh();
    await rulesStore.refresh();
    await vi.waitFor(() =>
      expect(
        screen.getAllByText(/Couldn't refresh the (requests|rules)/),
      ).toHaveLength(2),
    );
    pending.status = "failed";
    rulesStore.status = "failed";
    expect(
      await screen.findByText(
        /Couldn't read the requests\. Puddle's service isn't answering\./,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        /Couldn't read the rules\. Puddle's service isn't answering\./,
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText("Nothing is waiting for demo.")).toBeNull();
    expect(screen.queryByText(/No rule applies/)).toBeNull();
  });

  it("warns above the lists when a refresh failed after rows were read", async () => {
    inbox.add(
      request(1, { workspace: "demo" as never, host: "a.example.com" }),
      "example.com",
    );
    rules.rules = [
      rule(1, { scope: { type: "workspace", workspace: "demo" } }),
    ];
    await load();
    render(Network);
    await screen.findByText("a.example.com");
    inbox.down = true;
    rules.down = true;
    await pending.refresh();
    await rulesStore.refresh();
    await vi.waitFor(() =>
      expect(
        screen.getAllByText(/Couldn't refresh the (requests|rules)/),
      ).toHaveLength(2),
    );
  });

  it("allows a request with one click, and the row leaves", async () => {
    inbox.add(
      request(1, { workspace: "demo" as never, host: "a.example.com" }),
      "example.com",
    );
    await load();
    render(Network);
    await fireEvent.click(
      await screen.findByRole("button", { name: /^Allow/ }),
    );
    await vi.waitFor(() =>
      expect(
        screen.getByText("Nothing is waiting for demo."),
      ).toBeInTheDocument(),
    );
    expect(inbox.calls).toContain("POST /api/pending/{id}/approve");
  });

  it("shows the rules that apply, read-only, and links to the rules page", async () => {
    rules.rules = [
      rule(1, {
        pattern: "mine.example.com",
        scope: { type: "workspace", workspace: "demo" },
      }),
      rule(2, { pattern: "everyone.example.com" }),
      rule(3, {
        pattern: "theirs.example.com",
        scope: { type: "workspace", workspace: "other" },
      }),
    ];
    await load();
    render(Network);
    const table = await screen.findByRole("table");
    expect(table).toHaveTextContent("mine.example.com");
    expect(table).toHaveTextContent("everyone.example.com");
    expect(table).not.toHaveTextContent("theirs.example.com");
    expect(within(table).queryAllByRole("button")).toHaveLength(0);
    expect(screen.getByRole("link", { name: "All rules" })).toHaveAttribute(
      "href",
      "/rules",
    );
  });

  it("lists the rules on method and path that apply, read-only, with their warning", async () => {
    requestRules.rules = [
      requestRule(1, {
        host: "api.example.com",
        methods: ["DELETE"],
        path: "/repos/**",
        workspace: "demo",
      }),
      requestRule(2, {
        host: "*.example.org",
        warning: "example.org is decrypted for this rule.",
      }),
      requestRule(3, { host: "theirs.example.com", workspace: "other" }),
    ];
    await load();
    render(Network);
    const section = await screen.findByRole("region", {
      name: "Rules on method and path",
    });
    const table = within(section).getByRole("table");
    expect(table).toHaveTextContent("api.example.com");
    expect(table).toHaveTextContent("DELETE");
    expect(table).toHaveTextContent("/repos/**");
    expect(table).toHaveTextContent("*.example.org");
    expect(table).toHaveTextContent("example.org is decrypted for this rule.");
    expect(table).not.toHaveTextContent("theirs.example.com");
    expect(within(section).queryAllByRole("button")).toHaveLength(0);
    expect(
      within(section).getByRole("link", { name: "All method and path rules" }),
    ).toHaveAttribute("href", "/rules");
  });

  it("says so when no rule on method and path applies", async () => {
    requestRules.rules = [requestRule(3, { workspace: "other" })];
    await load();
    render(Network);
    expect(
      await screen.findByText("No rule on method and path applies to demo."),
    ).toBeInTheDocument();
  });

  it("says so when no rule applies yet", async () => {
    await load();
    render(Network);
    expect(
      await screen.findByText(/No rule applies to demo yet/),
    ).toBeInTheDocument();
  });

  it("shows nothing for a workspace it doesn't know", async () => {
    url.id = "nope";
    await load();
    const { container } = render(Network);
    expect(container.textContent?.trim()).toBe("");
  });
});

describe("the settings tab", () => {
  beforeEach(() => {
    url.pathname = "/workspaces/demo/settings";
  });

  async function open() {
    await load();
    render(Settings);
    return screen.findByLabelText(
      "Memory for this workspace",
    ) as Promise<HTMLSelectElement>;
  }

  it("shows each setting with where its value comes from", async () => {
    const memory = await open();
    expect(memory.value).toBe("");
    expect(
      screen.getByText(/A change applies at the next restart/),
    ).toBeInTheDocument();
    expect(screen.getAllByText("global setting").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Puddle's default").length).toBeGreaterThan(0);
    for (const label of [
      "This computer (loopback)",
      "Private networks",
      "Link-local",
      "Cloud metadata",
      "Other special ranges",
      "Pages reading the clipboard",
    ]) {
      expect(screen.getByLabelText(label)).toBeInTheDocument();
    }
    expect(document.getElementById("local-destinations")).not.toBeNull();
  });

  it("lists stored fields it doesn't know, only when there are some", async () => {
    await open();
    expect(screen.queryByText(/fields this Puddle doesn't know/)).toBeNull();
    cleanup();
    api.unknown = { demo: ["clipboard_reed"] };
    await open();
    expect(
      screen.getByText(/fields this Puddle doesn't know/),
    ).toHaveTextContent("clipboard_reed");
  });

  it("saves a memory choice at once and says the next start applies it", async () => {
    const memory = await open();
    await fireEvent.change(memory, { target: { value: "16384" } });
    await screen.findByText("Memory saved.");
    expect(api.overrides["demo"]?.memory).toBe(16_384);
    expect(
      await screen.findByText(
        "The new memory applies the next time the workspace starts.",
      ),
    ).toBeInTheDocument();
    await fireEvent.change(memory, { target: { value: "" } });
    await vi.waitFor(() => expect(api.overrides["demo"]?.memory).toBeNull());
  });

  it("saves a CPU count at once; a running workspace is told it keeps its count until the next start", async () => {
    await open();
    const cpus = screen.getByLabelText(
      "CPUs for this workspace",
    ) as HTMLSelectElement;
    expect(cpus.value).toBe("");
    expect(cpus.options[0]?.text).toBe("Use the global default (1 CPU)");
    expect(
      screen.getByText(/A running workspace keeps the count it started with/),
    ).toBeInTheDocument();
    await fireEvent.change(cpus, { target: { value: "4" } });
    await screen.findByText("CPUs saved.");
    expect(api.overrides["demo"]?.cpus).toBe(4);
    expect(
      await screen.findByText(/The new CPU count applies the next time/),
    ).toBeInTheDocument();
    await fireEvent.change(cpus, { target: { value: "" } });
    await vi.waitFor(() => expect(api.overrides["demo"]?.cpus).toBeNull());
  });

  it("says nothing of a restart for a stopped workspace and puts the CPU choice back when the save is refused", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    await open();
    const cpus = screen.getByLabelText(
      "CPUs for this workspace",
    ) as HTMLSelectElement;
    await fireEvent.change(cpus, { target: { value: "2" } });
    await screen.findByText("CPUs saved.");
    expect(screen.queryByText(/The new CPU count/)).toBeNull();
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "no",
    });
    await fireEvent.change(cpus, { target: { value: "8" } });
    await vi.waitFor(() => expect(cpus.value).toBe("2"));
  });

  it("doesn't mention a restart for a workspace that isn't running", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    const memory = await open();
    await fireEvent.change(memory, { target: { value: "4096" } });
    await screen.findByText("Memory saved.");
    expect(screen.queryByText(/applies the next time/)).toBeNull();
  });

  it("turns direct SSH off or back to the global default at once, and asks the trust text before on", async () => {
    await open();
    const select = screen.getByLabelText(
      "Allow direct SSH",
    ) as HTMLSelectElement;
    expect(select.value).toBe("inherit");
    await fireEvent.change(select, { target: { value: "on" } });
    expect(workspaceActions.trustOpen).toBe(true);
    expect(workspaceActions.trustFor?.name).toBe("demo");
    expect(api.overrides["demo"]?.direct_ssh ?? null).toBeNull();
    expect(select.value).toBe("inherit");
    await fireEvent.change(select, { target: { value: "off" } });
    await screen.findByText("Allow direct SSH saved.");
    expect(api.overrides["demo"]?.direct_ssh).toBe(false);
    await fireEvent.change(select, { target: { value: "inherit" } });
    await vi.waitFor(() =>
      expect(api.overrides["demo"]?.direct_ssh).toBeNull(),
    );
  });

  it("keeps logins by default, turns capture off or back to the global default, and says the next start applies it", async () => {
    await open();
    const select = screen.getByLabelText(
      "Keep logins made in this workspace",
    ) as HTMLSelectElement;
    expect(select.value).toBe("inherit");
    expect(
      screen.getByRole("option", { name: "Use the global setting (on)" }),
    ).toBeInTheDocument();
    await fireEvent.change(select, { target: { value: "off" } });
    await screen.findByText("Login capture saved.");
    expect(api.overrides["demo"]?.capture_logins).toBe(false);
    expect(
      await screen.findByText(
        "The new login setting applies the next time the workspace starts.",
      ),
    ).toBeInTheDocument();
    await fireEvent.change(select, { target: { value: "inherit" } });
    await vi.waitFor(() =>
      expect(api.overrides["demo"]?.capture_logins).toBeNull(),
    );
  });

  it("turns decrypting every host on and off at once and saves the never-decrypt names", async () => {
    await open();
    const box = (await screen.findByRole("switch", {
      name: "Decrypt every host",
    })) as HTMLInputElement;
    expect(box.checked).toBe(false);
    await fireEvent.click(box);
    await screen.findByText("Puddle now decrypts every host.");
    expect(api.interception["demo"]?.decrypt_all).toBe(true);
    const names = screen.getByRole("textbox", {
      name: "Never decrypt in this workspace",
    });
    await fireEvent.input(names, {
      target: { value: "Bank.example\n*.corp.test" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Keep names" }));
    await screen.findByText("Saved.");
    expect(api.interception["demo"]?.never_decrypt).toEqual([
      "*.corp.test",
      "bank.example",
    ]);
    await fireEvent.click(box);
    await screen.findByText("Puddle decrypts only hosts with a credential.");
    expect(api.interception["demo"]?.decrypt_all).toBe(false);
  });

  it("names the global never-decrypt list read-only, and puts the switch back when the save is refused", async () => {
    api.globalNeverDecrypt = ["bank.example"];
    await open();
    const box = (await screen.findByRole("switch", {
      name: "Decrypt every host",
    })) as HTMLInputElement;
    expect(
      screen.getByText(/Never decrypted in any workspace/),
    ).toHaveTextContent("bank.example");
    api.refuse.set("PUT /api/workspaces/{id}/interception", {
      status: 500,
      message: "disk full",
    });
    await fireEvent.click(box);
    expect(await screen.findByRole("alert")).toHaveTextContent("Disk full.");
    expect(box.checked).toBe(false);
  });

  it("lists the hosts a tool refused Puddle's certificate for and decrypts them again", async () => {
    api.learned["demo"] = [
      { host: "pinned.example", expires_in_secs: 600 },
      { host: "other.example", expires_in_secs: 7200 },
    ];
    await open();
    const group = await screen.findByRole("group", {
      name: "Left alone after a tool refused Puddle's certificate",
    });
    await screen.findByText("pinned.example");
    expect(group).toHaveTextContent("decrypted again by itself in 10 minutes");
    expect(group).toHaveTextContent("2 hours");
    await fireEvent.click(
      screen.getByRole("button", { name: "Decrypt pinned.example again" }),
    );
    await vi.waitFor(() =>
      expect(group).not.toHaveTextContent("pinned.example"),
    );
    expect(group).toHaveTextContent("other.example");
    await fireEvent.click(
      screen.getByRole("button", { name: "Decrypt all again" }),
    );
    await vi.waitFor(() => expect(group).toHaveTextContent("None."));
  });

  it("says why a host could not be decrypted again and keeps the list", async () => {
    api.learned["demo"] = [{ host: "pinned.example", expires_in_secs: 600 }];
    await open();
    await screen.findByText("pinned.example");
    api.refuse.set("DELETE /api/workspaces/{id}/interception/learned/{host}", {
      status: 500,
      message: "the list is locked",
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Decrypt pinned.example again" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The list is locked.",
    );
    expect(screen.getByText("pinned.example")).toBeInTheDocument();
  });

  it("says when the learned list can't be read", async () => {
    api.refuse.set("GET /api/workspaces/{id}/interception/learned", {
      status: 500,
      message: "list broken",
    });
    await open();
    expect(await screen.findByText("List broken.")).toBeInTheDocument();
  });

  it("says when the decrypt setting can't be read", async () => {
    api.refuse.set("GET /api/workspaces/{id}/interception", {
      status: 500,
      message: "store broken",
    });
    await open();
    expect(await screen.findByText("Store broken.")).toBeInTheDocument();
    expect(
      screen.queryByRole("switch", { name: "Decrypt every host" }),
    ).toBeNull();
  });

  it("doesn't mention a restart for login capture when the workspace isn't running, and puts the choice back when the save is refused", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    await open();
    const select = screen.getByLabelText(
      "Keep logins made in this workspace",
    ) as HTMLSelectElement;
    await fireEvent.change(select, { target: { value: "on" } });
    await screen.findByText("Login capture saved.");
    expect(screen.queryByText(/applies the next time/)).toBeNull();
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "no",
    });
    await fireEvent.change(select, { target: { value: "off" } });
    await vi.waitFor(() => expect(select.value).toBe("on"));
  });

  it("lists the kept logins on the settings page, forgets one after a confirmation, and ends its notice", async () => {
    api.kept["demo"] = {
      store_readable: true,
      logins: [
        { service: "claude", name: "Claude Code", since: 1_761_480_000_000 },
        { service: "github", name: "GitHub", since: null },
      ],
    };
    notices.add({
      key: "login:demo:Claude Code:capture_off",
      tone: "warning",
      title: "Claude Code login in demo can't be used.",
    });
    await open();
    const list = await screen.findByRole("region", { name: "Kept logins" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
    expect(list).toHaveTextContent("Claude Code");
    expect(list).toHaveTextContent("Kept since a time Puddle did not record.");
    expect(list.textContent).not.toMatch(/sk-|gho_|token/i);

    await fireEvent.click(
      within(list).getByRole("button", {
        name: "Forget the Claude Code login",
      }),
    );
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveAccessibleName(
      "Forget the Claude Code login in demo?",
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Forget login" }),
    );
    await vi.waitFor(() =>
      expect(screen.queryByRole("alertdialog")).toBeNull(),
    );
    expect(api.calls).toContain("DELETE /api/workspaces/{id}/logins/{service}");
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    expect(list).toHaveTextContent("Forgot the Claude Code login.");
    expect(
      notices.items.some((n) => n.key === "login:demo:Claude Code:capture_off"),
    ).toBe(false);
  });

  it("keeps the row and says why when the forget is refused, and Cancel closes the dialog", async () => {
    api.kept["demo"] = {
      store_readable: true,
      logins: [{ service: "claude", name: "Claude Code", since: null }],
    };
    api.refuse.set("DELETE /api/workspaces/{id}/logins/{service}", {
      status: 500,
      message: "boom",
    });
    await open();
    const list = await screen.findByRole("region", { name: "Kept logins" });
    await fireEvent.click(
      within(list).getByRole("button", {
        name: "Forget the Claude Code login",
      }),
    );
    const dialog = await screen.findByRole("alertdialog");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Forget login" }),
    );
    expect(await within(dialog).findByText("Boom.")).toBeInTheDocument();
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await vi.waitFor(() =>
      expect(screen.queryByRole("alertdialog")).toBeNull(),
    );
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
  });

  it("says so when no login is kept, when the list can't be read, and when the credential store did not answer", async () => {
    await open();
    expect(
      await screen.findByText(/keeps no logins for this workspace/),
    ).toBeInTheDocument();
    cleanup();
    api.kept["demo"] = { logins: [], store_readable: false };
    await open();
    expect(
      await screen.findByText(/couldn't read this computer's credential store/),
    ).toBeInTheDocument();
    cleanup();
    api.kept = {};
    api.refuse.set("GET /api/workspaces/{id}/logins", {
      status: 500,
      message: "boom",
    });
    await open();
    expect(
      await screen.findByText("Couldn't read the kept logins."),
    ).toBeInTheDocument();
    expect(screen.getByText("Boom.")).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(
      await screen.findByText(/keeps no logins for this workspace/),
    ).toBeInTheDocument();
  });

  it("reads the kept logins again when the credential store did not answer and the user retries", async () => {
    api.kept["demo"] = { logins: [], store_readable: false };
    await open();
    await screen.findByText(/couldn't read this computer's credential store/);
    api.kept["demo"] = { logins: [], store_readable: true };
    await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(
      await screen.findByText(/keeps no logins for this workspace/),
    ).toBeInTheDocument();
  });

  it("saves a local toggle and the clipboard, keeping the others as they were", async () => {
    await open();
    await fireEvent.change(screen.getByLabelText("Private networks"), {
      target: { value: "on" },
    });
    await screen.findByText("Private networks saved.");
    expect(api.overrides["demo"]?.local_toggles.private).toBe(true);
    expect(api.overrides["demo"]?.local_toggles.loopback).toBeNull();
    await fireEvent.change(
      screen.getByLabelText("Pages reading the clipboard"),
      {
        target: { value: "deny" },
      },
    );
    await screen.findByText("Clipboard saved.");
    expect(api.overrides["demo"]?.clipboard_read).toBe("deny");
    expect(api.overrides["demo"]?.local_toggles.private).toBe(true);
    await fireEvent.change(
      screen.getByLabelText("Pages reading the clipboard"),
      {
        target: { value: "inherit" },
      },
    );
    await vi.waitFor(() =>
      expect(api.overrides["demo"]?.clipboard_read).toBeNull(),
    );
    await fireEvent.change(screen.getByLabelText("Private networks"), {
      target: { value: "off" },
    });
    await vi.waitFor(() =>
      expect(api.overrides["demo"]?.local_toggles.private).toBe(false),
    );
  });

  it("puts the choice back and says why when the save is refused", async () => {
    const memory = await open();
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "memory is too small",
    });
    await fireEvent.change(memory, { target: { value: "4096" } });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "memory is too small",
    );
    expect(memory.value).toBe("");
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "no",
    });
    const toggle = screen.getByLabelText("Link-local") as HTMLSelectElement;
    await fireEvent.change(toggle, { target: { value: "on" } });
    await vi.waitFor(() => expect(toggle.value).toBe("inherit"));
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "no",
    });
    const clipboard = screen.getByLabelText(
      "Pages reading the clipboard",
    ) as HTMLSelectElement;
    await fireEvent.change(clipboard, { target: { value: "allow" } });
    await vi.waitFor(() => expect(clipboard.value).toBe("inherit"));
  });

  it("says when the settings can't be read, and while they load", async () => {
    await load();
    api.refuse.set("GET /api/settings/workspaces/{workspace}", {
      status: 500,
      message: "x",
    });
    render(Settings);
    expect(screen.getByText(/Loading settings/)).toBeInTheDocument();
    expect(
      await screen.findByText(/Couldn't read this workspace's settings/),
    ).toBeInTheDocument();
  });

  it("shows nothing for a workspace it doesn't know", async () => {
    url.id = "nope";
    await load();
    const { container } = render(Settings);
    expect(container.textContent?.trim()).toBe("");
  });
});
