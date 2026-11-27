// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

const { h, url, goto } = await vi.hoisted(() => import("./detail-harness.ts"));
vi.mock("$app/state", async () => {
  const { navigations } = await import("./detail-navigation.ts");
  return {
    page: {
      get url() {
        return new URL(`http://127.0.0.1${url.pathname}`);
      },
      get params() {
        navigations.get("n");
        return { id: url.id };
      },
    },
  };
});
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

import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaceActions } from "#lib/stores/workspace-actions.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import {
  checkout,
  cleanCheck,
  dirtyCheck,
  listing,
} from "#lib/testing/fake-workspaces.ts";
import Overview from "./+page.svelte";
import Network from "./network/+page.svelte";
import Settings from "./settings/+page.svelte";
import {
  findHost,
  getHost,
  installReset,
  load,
  openSettings,
  queryHost,
} from "./detail-setup.ts";

const { inbox, api, workspace, request } = h;

installReset();

describe("the overview", () => {
  it("shows when it was created, what waits, the repositories and what listens, and nothing of its id or state", async () => {
    inbox.add(request(1, { workspace: "w-demo" as never }), "example.com");
    const entry = {
      entry: "e-demo",
      url: "https://github.com/acme/demo.git",
      directory: "demo",
      branch: null,
    };
    api.checkouts["w-demo"] = listing([checkout("demo", { entry })]);
    await load();
    render(Overview);
    expect(await screen.findByText(/^Created/)).toBeInTheDocument();
    expect(
      await screen.findByRole("list", { name: "Checkouts of demo" }),
    ).toHaveTextContent("https://github.com/acme/demo.git");
    expect(
      screen.getByRole("link", { name: "Open a terminal" }),
    ).toHaveAttribute("href", "/workspaces/w-demo/terminal");
    expect(
      await screen.findByRole("heading", { name: /Waiting for you/ }),
    ).toBeInTheDocument();
    expect(screen.queryByText("State")).toBeNull();
    expect(screen.queryByText("Workspace id")).toBeNull();
    expect(screen.queryByText(/w-demo/)).toBeNull();
    expect(screen.queryByRole("heading", { name: "Network" })).toBeNull();
    expect(
      screen.queryByRole("heading", { name: "Delete workspace" }),
    ).toBeNull();
  });

  it("decides one waiting request on the Overview and leaves the others", async () => {
    inbox.add(
      request(1, { workspace: "w-demo" as never, host: "a.example.com" }),
      "example.com",
    );
    inbox.add(
      request(2, { workspace: "w-demo" as never, host: "b.example.com" }),
      "example.com",
    );
    inbox.add(
      request(3, { workspace: "other" as never, host: "c.example.org" }),
      "example.org",
    );
    await load();
    render(Overview);
    await findHost("a.example.com");
    expect(queryHost("c.example.org")).toBeNull();
    const row = getHost("a.example.com").closest("li") as HTMLElement;
    await fireEvent.click(within(row).getByRole("button", { name: /^Allow/ }));
    await vi.waitFor(() => expect(queryHost("a.example.com")).toBeNull());
    expect(getHost("b.example.com")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Allow all" })).toBeNull();
  });

  it("offers Allow all and Deny all from two waiting requests, asks first, and decides each one", async () => {
    inbox.add(
      request(1, { workspace: "w-demo" as never, host: "a.example.com" }),
      "example.com",
    );
    inbox.add(
      request(2, { workspace: "w-demo" as never, host: "b.example.com" }),
      "example.com",
    );
    inbox.add(
      request(3, { workspace: "other" as never, host: "c.example.org" }),
      "example.org",
    );
    await load();
    render(Overview);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Allow all" }),
    );
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Allow all 2 requests?");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Allow all" }),
    );
    await vi.waitFor(() => expect(queryHost("a.example.com")).toBeNull());
    expect(queryHost("b.example.com")).toBeNull();
    expect(inbox.calls.filter((c) => c.includes("approve"))).toHaveLength(2);
    expect(inbox.calls.filter((c) => c.includes("deny"))).toHaveLength(0);
    expect(toasts.items.at(-1)?.message).toBe("Allowed 2 requests for demo.");
    expect(inbox.open.map((r) => r.request.host)).toEqual(["c.example.org"]);
  });

  it("denies all waiting requests of the workspace after a confirm, and Cancel decides nothing", async () => {
    inbox.add(
      request(1, { workspace: "w-demo" as never, host: "a.example.com" }),
      "example.com",
    );
    inbox.add(
      request(2, { workspace: "w-demo" as never, host: "b.example.com" }),
      "example.com",
    );
    await load();
    render(Overview);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Deny all" }),
    );
    let dialog = await screen.findByRole("alertdialog");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    expect(inbox.calls.filter((c) => c.includes("deny"))).toHaveLength(0);
    await vi.waitFor(() =>
      expect(screen.queryByRole("alertdialog")).toBeNull(),
    );
    await fireEvent.click(screen.getByRole("button", { name: "Deny all" }));
    dialog = await screen.findByRole("alertdialog");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Deny all" }),
    );
    await vi.waitFor(() =>
      expect(inbox.calls.filter((c) => c.includes("deny"))).toHaveLength(2),
    );
    await vi.waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe("Denied 2 requests for demo."),
    );
  });

  it("opens the more-choices popover of a waiting request on the Overview", async () => {
    inbox.add(
      request(1, { workspace: "w-demo" as never, host: "a.example.com" }),
      "example.com",
    );
    await load();
    render(Overview);
    const more = await screen.findByRole("button", {
      name: "More choices for a.example.com",
    });
    await fireEvent.click(more);
    await vi.waitFor(() =>
      expect(more).toHaveAttribute("aria-expanded", "true"),
    );
  });

  it("counts a decision that could not be made, and says so in the summary", async () => {
    inbox.add(
      request(1, { workspace: "w-demo" as never, host: "a.example.com" }),
      "example.com",
    );
    inbox.add(
      request(2, { workspace: "w-demo" as never, host: "b.example.com" }),
      "example.com",
    );
    await load();
    render(Overview);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Allow all" }),
    );
    inbox.down = true;
    const dialog = await screen.findByRole("alertdialog");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Allow all" }),
    );
    await vi.waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe(
        "Allowed 0 requests for demo; 2 requests could not be decided.",
      ),
    );
    expect(getHost("a.example.com")).toBeInTheDocument();
  });

  it("keeps the times on waiting requests current as the minutes pass", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    try {
      inbox.add(
        request(1, {
          workspace: "w-demo" as never,
          host: "a.example.com",
          first_seen: Date.now() - 60_000,
          last_seen: Date.now() - 60_000,
        }),
        "example.com",
      );
      await load();
      render(Overview);
      render(Network);
      await findHost("a.example.com");
      const before = document.querySelector("li.req time")?.textContent;
      await vi.advanceTimersByTimeAsync(31 * 60_000);
      expect(document.querySelector("li.req time")?.textContent).not.toBe(
        before,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("lists what listens in a running workspace, with where it is forwarded", async () => {
    const real = api.GET;
    api.GET = (async (...args: Parameters<typeof real>) => {
      if (args[0] === "/api/workspaces/{id}/ports") {
        return {
          data: {
            running: true,
            ports: [
              {
                guest_port: 3000,
                host_port: 3000,
                forwarded: true,
                pinned: false,
                listening: true,
                problem: null,
              },
              {
                guest_port: 5432,
                host_port: null,
                forwarded: false,
                pinned: false,
                listening: false,
                problem: null,
              },
            ],
          },
        };
      }
      return real(...args);
    }) as typeof real;
    try {
      await load();
      render(Overview);
      const section = (
        await screen.findByRole("heading", { name: "Serving now" })
      ).closest("section") as HTMLElement;
      expect(section).toHaveTextContent(":3000");
      expect(section).toHaveTextContent("Open on 127.0.0.1:3000");
      expect(section).not.toHaveTextContent("5432");
      expect(
        within(section).getByRole("link", { name: "Forward a port" }),
      ).toHaveAttribute("href", "/workspaces/w-demo/settings/ports");
    } finally {
      api.GET = real;
    }
  });

  it("shows no serving list for a workspace that listens on nothing", async () => {
    await load();
    render(Overview);
    await screen.findByText("Get in");
    expect(screen.queryByRole("heading", { name: "Serving now" })).toBeNull();
  });

  it("says nothing waits for the workspace when it is so", async () => {
    await load();
    render(Overview);
    expect(
      await screen.findByText("Nothing is waiting for demo."),
    ).toBeInTheDocument();
  });

  it("shows an out-of-memory notice that can be dismissed", async () => {
    await load();
    render(Overview);
    await screen.findByText("Get in");
    expect(screen.queryByRole("alert")).toBeNull();
    workspaces.handleEvent({
      type: "oom_kill",
      workspace: "w-demo",
      pid: 77,
      process: "node",
    });
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Out of memory");
    expect(alert).toHaveTextContent("node");
    expect(alert).toHaveTextContent("process 77");
    expect(
      within(alert).getByRole("link", { name: "Change memory" }),
    ).toHaveAttribute("href", "/workspaces/w-demo/settings");
    await fireEvent.click(
      within(alert).getByRole("button", { name: "Dismiss" }),
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("reclaims space on a running workspace, and not while it is changing", async () => {
    await openSettings();
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

  it("shows the root disk and the volume in words, and says which one is nearly full", async () => {
    api.list = [
      workspace("demo", {
        status: "stopped",
        root_disk_size_mib: 4096,
        root_disk_used_mib: 3900,
        disk_warnings: ["root"],
      }),
    ];
    await load();
    render(Overview);
    const banner = await screen.findByTestId("disk-warning-root");
    expect(banner).toHaveAttribute("role", "alert");
    expect(banner).toHaveTextContent("Root disk nearly full: 95 % used.");
    expect(banner).toHaveTextContent("the volume and your work are untouched");
    expect(screen.queryByTestId("disk-warning-volume")).toBeNull();
    cleanup();
    render(Settings);
    expect(
      await screen.findByText("3.8 GiB of 4 GiB used (nearly full)"),
    ).toBeInTheDocument();
    expect(screen.getByText("2 GiB of 32 GiB used")).toBeInTheDocument();
  });

  it("resets the root disk of a stopped workspace after a confirm that says what is lost", async () => {
    api.list = [
      workspace("demo", {
        status: "stopped",
        root_disk_size_mib: 4096,
        root_disk_used_mib: 4000,
        disk_warnings: ["root"],
      }),
    ];
    await load();
    render(Overview);
    const banner = await screen.findByTestId("disk-warning-root");
    await fireEvent.click(
      within(banner).getByRole("button", { name: "Reset root disk…" }),
    );
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Reset the root disk of demo?");
    expect(dialog).toHaveTextContent(
      "packages reinstall and caches rebuild on the next start",
    );
    expect(dialog).toHaveTextContent(
      "The volume, with your repositories and files, is untouched.",
    );
    expect(api.calls).not.toContain(
      "POST /api/workspaces/{id}/reset-root-disk",
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Reset root disk" }),
    );
    await vi.waitFor(() =>
      expect(api.calls).toContain("POST /api/workspaces/{id}/reset-root-disk"),
    );
  });

  it("makes the root disk bigger from the warning of a running workspace, without a reset", async () => {
    api.list = [
      workspace("demo", {
        status: "running",
        root_disk_size_mib: 4096,
        root_disk_used_mib: 4000,
        disk_warnings: ["root"],
      }),
    ];
    await load();
    render(Overview);
    const banner = await screen.findByTestId("disk-warning-root");
    await fireEvent.click(
      within(banner).getByRole("button", { name: "Make root disk bigger…" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Make the root disk of demo bigger");
    const input = within(dialog).getByRole("textbox", {
      name: /New size in GiB/,
    });
    expect(input).toHaveValue("14");
    await fireEvent.input(input, { target: { value: "16" } });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Make bigger" }),
    );
    await vi.waitFor(() =>
      expect(api.calls).toContain("POST /api/workspaces/{id}/grow-root-disk"),
    );
    expect(api.bodies.at(-1)).toEqual({ size_mib: 16_384 });
    expect(api.calls).not.toContain(
      "POST /api/workspaces/{id}/reset-root-disk",
    );
  });

  it("offers the grow in the Advanced group of Settings, and not while the workspace is busy", async () => {
    api.list = [
      workspace("demo", { status: "stopped", root_disk_size_mib: 4096 }),
    ];
    await openSettings();
    const grow = await screen.findByRole("button", {
      name: "Make root disk bigger…",
    });
    expect(grow).toBeEnabled();
    api.list = [
      workspace("demo", {
        status: "stopped",
        root_disk_size_mib: 4096,
        busy: "reclaiming",
      }),
    ];
    await workspaces.refresh();
    await vi.waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Make root disk bigger…" }),
      ).toBeDisabled(),
    );
  });

  it("offers the reset next to Reclaim space even when no disk is nearly full", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    await openSettings();
    await fireEvent.click(
      await screen.findByRole("button", { name: "Reset root disk…" }),
    );
    expect(await screen.findByRole("alertdialog")).toHaveTextContent(
      "Reset the root disk of demo?",
    );
  });

  it("does not offer the reset to a running workspace and asks to stop it first", async () => {
    api.list = [
      workspace("demo", {
        status: "running",
        root_disk_size_mib: 4096,
        root_disk_used_mib: 4000,
        disk_warnings: ["root"],
      }),
    ];
    await load();
    render(Overview);
    const banner = await screen.findByTestId("disk-warning-root");
    expect(
      within(banner).getByRole("button", { name: "Reset root disk…" }),
    ).toBeDisabled();
    expect(
      within(banner).getByText("Stop the workspace to reset its root disk."),
    ).toBeInTheDocument();
  });

  it("warns about a full volume without offering a reset", async () => {
    api.list = [
      workspace("demo", {
        status: "stopped",
        disk_used_mib: 32_000,
        disk_warnings: ["volume"],
      }),
    ];
    await load();
    render(Overview);
    const banner = await screen.findByTestId("disk-warning-volume");
    expect(banner).toHaveTextContent("Volume nearly full: 97 % used.");
    expect(banner).toHaveTextContent("cannot grow");
    expect(within(banner).queryByRole("button")).toBeNull();
    cleanup();
    render(Settings);
    expect(
      await screen.findByText("31.3 GiB of 32 GiB used (nearly full)"),
    ).toBeInTheDocument();
  });

  it("shows no root disk when the host cannot say", async () => {
    api.list = [
      workspace("demo", { root_disk_size_mib: null, root_disk_used_mib: null }),
    ];
    await openSettings();
    expect(screen.queryByText("Root disk")).toBeNull();
  });

  it("can't delete a running workspace and says why", async () => {
    await openSettings();
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
    api.check = dirtyCheck("w-demo");
    await openSettings();
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
        key: "editor-install:w-demo",
        workspace: "w-demo",
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
    await screen.findByText("Get in");
    expect(screen.queryByText(/could not be installed/)).toBeNull();
    expect(screen.queryByText(/could not update/)).toBeNull();
    expect(screen.queryByText(/runs as root/)).toBeNull();
  });

  it("says on the workspace's page that its browser editor runs as root for want of the session user", async () => {
    api.problems = [
      {
        key: "editor-session-user:w-demo",
        workspace: "w-demo",
        title:
          "The browser editor in demo runs as root: its user agent is missing in the guest",
        detail: "The editor opens and works, but the files belong to root.",
      },
      {
        key: "editor-session-user:w-other",
        workspace: "w-other",
        title: "The browser editor in other runs as root: no",
        detail: "x",
      },
    ];
    await load();
    render(Overview);
    expect(
      await screen.findByText(
        /in demo runs as root: its user agent is missing in the guest/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/The editor opens and works, but the files belong/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/in other runs as root/)).toBeNull();
  });

  it("says on the workspace's page that its browser editor could not update, and that the editor keeps working", async () => {
    api.problems = [
      {
        key: "editor-update:w-demo",
        workspace: "w-demo",
        title:
          "The browser editor in demo could not update: the newer build exited before it started, so the previous one runs again",
        detail: "The editor keeps working on the build it has.",
      },
      {
        key: "editor-update:other",
        workspace: "other",
        title: "The browser editor in other could not update: no",
        detail: "x",
      },
    ];
    await load();
    render(Overview);
    expect(
      await screen.findByText(
        /in demo could not update: the newer build exited/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/The editor keeps working on the build it has/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/in other could not update/)).toBeNull();
  });

  it("says a workspace with no volume can be restored or deleted, and that deleting loses nothing", async () => {
    api.list = [workspace("demo", { status: "volume_missing" })];
    api.check = cleanCheck("w-demo", { volume_missing: true });
    await load();
    render(Overview);
    expect(await screen.findByText(/Its disk is gone/)).toBeInTheDocument();
    expect(
      screen.getByText(
        /delete the workspace under Danger in Settings; nothing is lost/,
      ),
    ).toBeInTheDocument();
    cleanup();
    render(Settings);
    expect(
      await screen.findByText(
        /deleting only removes the workspace from Puddle/,
      ),
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
