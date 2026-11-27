// SPDX-License-Identifier: AGPL-3.0-or-later
import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { h, url, goto } = await vi.hoisted(() => import("./detail-harness.ts"));
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
vi.mock("$app/navigation", () => ({ goto }));
vi.mock("#lib/stores/pending.svelte.ts", async (original) =>
  (await import("./detail-harness.ts")).pendingMock(await original()),
);
vi.mock("#lib/stores/workspaces.svelte.ts", async (original) =>
  (await import("./detail-harness.ts")).workspacesMock(await original()),
);
vi.mock("#lib/stores/rules.svelte.ts", async (original) =>
  (await import("./detail-harness.ts")).rulesMock(await original()),
);
vi.mock("#lib/stores/request-rules.svelte.ts", async (original) =>
  (await import("./detail-harness.ts")).requestRulesMock(await original()),
);
vi.mock("#lib/api/client.ts", () => ({ api: h.api }));

import { pending } from "#lib/stores/pending.svelte.ts";
import { rulesStore } from "#lib/stores/rules.svelte.ts";
import { workspaceActions } from "#lib/stores/workspace-actions.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import DetailLayout from "./+layout.svelte";
import Overview from "./+page.svelte";
import Network from "./network/+page.svelte";
import { findHost, installReset, load, queryHost } from "./detail-setup.ts";

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

installReset();

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
      ["Overview", "Terminal", "Network", "Git", "Settings"],
    );
    expect(tabs.getByRole("link", { name: "Overview" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(tabs.getByRole("link", { name: "Settings" })).toHaveAttribute(
      "href",
      "/workspaces/w-demo/settings",
    );
  });

  it("titles the page with the name the user gave, keeps the identifier in the paths, and opens the rename dialog", async () => {
    api.list = [workspace("demo", { status: "running", name: "Web shop" })];
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
      "/workspaces/w-demo/settings",
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Rename Web shop" }),
    );
    expect(workspaceActions.renameOpen).toBe(true);
    expect(workspaceActions.renaming?.id).toBe("w-demo");
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

  it("marks another tab, and counts what waits on the Overview tab", async () => {
    url.pathname = "/workspaces/w-demo/network";
    inbox.add(request(1, { workspace: "w-demo" as never }), "example.com");
    inbox.add(request(2, { workspace: "other" as never }), "example.com");
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    const network = await screen.findByRole("link", { name: "Network" });
    expect(network).toHaveAttribute("aria-current", "page");
    expect(network).not.toHaveTextContent("waiting");
    expect(screen.getByRole("link", { name: /Overview/ })).toHaveTextContent(
      "1 waiting",
    );
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

  it("stops the workspace and opens the connect step from the buttons in the header", async () => {
    await load();
    render(DetailLayout, { children: (() => {}) as never });
    await fireEvent.click(
      await screen.findByRole("button", { name: "Connect to demo" }),
    );
    expect(workspaceActions.connectOpen).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "Stop demo" }));
    await vi.waitFor(() =>
      expect(api.calls).toContain("POST /api/workspaces/{id}/stop"),
    );
  });

  it("dismisses the line of a step that failed", async () => {
    await load();
    workspaces.progress = {
      "w-demo": {
        step: "failed",
        detail: "the disk is full",
        repository: null,
        operation: "starting",
        failed: true,
      },
    };
    render(DetailLayout, { children: (() => {}) as never });
    await fireEvent.click(
      await screen.findByRole("button", { name: "Dismiss" }),
    );
    expect(workspaces.progress["w-demo"]).toBeUndefined();
  });

  it("says which clone of how many is running, from the workspace's repositories", async () => {
    api.list = [
      workspace("demo", {
        status: "stopped",
        repositories: [
          {
            entry: "e-web",
            url: "https://github.com/acme/web.git",
            directory: "web",
            branch: null,
          },
          {
            entry: "e-api",
            url: "https://github.com/acme/api.git",
            directory: "api",
            branch: null,
          },
        ],
      }),
    ];
    await load();
    workspaces.progress = {
      "w-demo": {
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

describe("the network tab", () => {
  beforeEach(() => {
    url.pathname = "/workspaces/w-demo/network";
  });

  it("lists this workspace's waiting requests on the Overview, newest first, and not another's", async () => {
    inbox.add(
      request(1, {
        workspace: "w-demo" as never,
        host: "old.example.com",
        first_seen: 1,
      }),
      "example.com",
    );
    inbox.add(
      request(2, {
        workspace: "w-demo" as never,
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
    render(Overview);
    await findHost("new.example.com");
    const hosts = [...document.querySelectorAll("li.req .host")].map((e) =>
      e.textContent?.replace(/:\d+$/, ""),
    );
    expect(hosts).toEqual(["new.example.com", "old.example.com"]);
    expect(queryHost("theirs.example.org")).toBeNull();
    expect(
      screen.getByRole("heading", { name: /Waiting for you/ }),
    ).toHaveTextContent("2");
  });

  it("points to the Overview instead of listing the waiting requests", async () => {
    inbox.add(
      request(1, { workspace: "w-demo" as never, host: "a.example.com" }),
      "example.com",
    );
    await load();
    render(Network);
    const note = (await screen.findByText(/request is waiting/)).closest(
      "p",
    ) as HTMLElement;
    expect(note).toHaveTextContent("1 request is waiting for demo.");
    expect(
      within(note).getByRole("link", { name: "Decide on the Overview" }),
    ).toHaveAttribute("href", "/workspaces/w-demo");
    expect(queryHost("a.example.com")).toBeNull();
  });

  it("says when nothing waits, and when requests are still loading", async () => {
    await workspaces.refresh();
    render(Overview);
    render(Network);
    expect(await screen.findByText(/Loading requests/)).toBeInTheDocument();
    expect(screen.getByText(/Loading rules/)).toBeInTheDocument();
    await load();
    expect(
      await screen.findByText("Nothing is waiting for demo."),
    ).toBeInTheDocument();
  });

  it("says a read failed instead of showing an empty list, and warns when a refresh failed", async () => {
    await load();
    render(Overview);
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
      request(1, { workspace: "w-demo" as never, host: "a.example.com" }),
      "example.com",
    );
    rules.rules = [
      rule(1, { scope: { type: "workspace", workspace: "w-demo" } }),
    ];
    await load();
    render(Overview);
    render(Network);
    await findHost("a.example.com");
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
      request(1, { workspace: "w-demo" as never, host: "a.example.com" }),
      "example.com",
    );
    await load();
    render(Overview);
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
        scope: { type: "workspace", workspace: "w-demo" },
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
        workspace: "w-demo",
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
