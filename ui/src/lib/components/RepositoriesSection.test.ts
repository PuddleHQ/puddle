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

const h = await vi.hoisted(async () => {
  const fakeIdentities = await import("#lib/testing/fake-identities.ts");
  const fakeWorkspaces = await import("#lib/testing/fake-workspaces.ts");
  const inbox = await import("#lib/testing/fake-inbox.ts");
  const identities = new fakeIdentities.FakeIdentities();
  const workspaces = new fakeWorkspaces.FakeWorkspaces();
  const toIdentities = (path: string) =>
    path.startsWith("/api/identities") ||
    path.startsWith("/api/credentials") ||
    path.endsWith("/git");
  // One client for both fakes: the section reads checkouts, the Git tab's data and the identities.
  type Call = (path: string, init?: unknown) => Promise<unknown>;
  const route = (method: "GET" | "POST" | "PUT" | "DELETE") =>
    ((path: string, init?: unknown) =>
      (
        (toIdentities(path) ? identities : workspaces) as unknown as Record<
          string,
          Call
        >
      )[method]!(path, init)) as never;
  return {
    identities,
    workspaces,
    source: new inbox.FakeSource(),
    client: {
      GET: route("GET"),
      POST: route("POST"),
      PUT: route("PUT"),
      DELETE: route("DELETE"),
    },
  };
});

vi.mock("#lib/api/client.ts", () => ({ api: h.client }));
vi.mock("#lib/stores/live.svelte.ts", async (original) => {
  const mod = await original<typeof import("#lib/stores/live.svelte.ts")>();
  return { ...mod, live: h.source };
});
vi.mock("#lib/stores/identities.svelte.ts", async (original) => {
  const mod =
    await original<typeof import("#lib/stores/identities.svelte.ts")>();
  return {
    ...mod,
    identities: new mod.IdentitiesStore({
      api: h.identities as never,
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
      api: h.workspaces as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
});

import { identities } from "#lib/stores/identities.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { credential, identity } from "#lib/testing/fake-identities.ts";
import { checkout, listing, workspace } from "#lib/testing/fake-workspaces.ts";
import type { Workspace } from "#lib/workspaces/model.ts";
import RepositoriesSection from "./RepositoriesSection.svelte";

const { identities: idApi, workspaces: api } = h;

const own = { url: "https://github.com/acme/api.git", directory: "api" };

beforeEach(() => {
  api.list = [];
  api.calls = [];
  api.bodies = [];
  api.down = false;
  api.checkouts = {};
  api.refuse.clear();
  idApi.identities = [
    identity(1, {
      label: "Work",
      credentials: [credential({ owners: ["acme"], rest: false })],
    }),
    identity(2, { label: "Personal", is_default: false }),
  ];
  idApi.git = { dev: { ids: [1], repos: [], push: true, pull: false } };
  identities.identities = [];
  identities.status = "loading";
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});
afterEach(cleanup);

const section = () =>
  within(screen.getByRole("region", { name: "Repositories" }));

async function mount(
  over: Partial<Workspace> = {},
  seen = listing([checkout("api", { entry: own })]),
) {
  const ws = workspace("dev", {
    status: "running",
    repositories: [own],
    ...over,
  });
  api.list = [ws];
  api.checkouts["dev"] = seen;
  workspaces.list = [ws];
  render(RepositoriesSection, { workspace: ws });
  await waitFor(() =>
    expect(screen.queryByText(/Reading the checkouts/)).toBeNull(),
  );
  return ws;
}

describe("the checkouts of a running workspace", () => {
  it("lists a managed checkout with its branch, address and serving identity", async () => {
    await mount();
    expect(section().getByText("api")).toBeInTheDocument();
    expect(section().getByText("On the list")).toBeInTheDocument();
    expect(section().getByText("main")).toBeInTheDocument();
    await waitFor(() =>
      expect(section().getByRole("link", { name: "Work" })).toHaveAttribute(
        "href",
        "/identities/1",
      ),
    );
    expect(
      section().getByRole("button", { name: "Remove api from the list" }),
    ).toBeInTheDocument();
  });

  it("marks one that is not managed as working as it is and offers Manage this", async () => {
    await mount({}, listing([checkout("scratch")]));
    expect(
      section().getByText("Not managed by Puddle: works as is"),
    ).toBeInTheDocument();
    expect(
      section().getByRole("button", { name: "Manage scratch" }),
    ).toBeInTheDocument();
  });

  it("gives the reason where Manage this is not offered", async () => {
    await mount(
      {},
      listing([
        checkout("local", {
          remote_url: null,
          manage: { offered: false, reason: "It has no remote." },
        }),
      ]),
    );
    expect(section().getByText("It has no remote.")).toBeInTheDocument();
    expect(
      section().queryByRole("button", { name: /Manage local/ }),
    ).toBeNull();
  });

  it("shows what the guest says as plain text", async () => {
    const hostile = '<img src=x onerror="window.__owned=1">';
    await mount(
      {},
      listing([checkout("evil", { branch: hostile, remote_url: hostile })]),
    );
    expect(
      section().getAllByText(hostile, { exact: false }).length,
    ).toBeGreaterThan(0);
    expect(document.querySelector("img")).toBeNull();
    expect((window as unknown as { __owned?: number }).__owned).toBeUndefined();
  });

  it("names an unreadable checkout instead of a branch", async () => {
    await mount(
      {},
      listing([checkout("broken", { unreadable: "permission denied" })]),
    );
    expect(section().getByText(/permission denied/)).toBeInTheDocument();
  });

  it("says when the list's address differs from the disk's", async () => {
    await mount(
      {},
      listing([
        checkout("api", {
          entry: own,
          remote_url: "https://github.com/other/api.git",
        }),
      ]),
    );
    expect(section().getByText(/The list says/)).toBeInTheDocument();
  });

  it("counts the checkouts it does not show", async () => {
    await mount({}, listing([checkout("a")], { more: 3 }));
    expect(section().getByText(/And 3 more checkouts/)).toBeInTheDocument();
  });
});

describe("Manage this", () => {
  it("sends the address the user saw with the request and puts the checkout on the list", async () => {
    await mount({ repositories: [] }, listing([checkout("scratch")]));
    await fireEvent.click(
      section().getByRole("button", { name: "Manage scratch" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Manage scratch",
    });
    expect(
      within(dialog).getByText("https://github.com/acme/scratch.git"),
    ).toBeInTheDocument();
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Manage this" }),
    );
    await waitFor(() =>
      expect(api.calls).toContain("POST /api/workspaces/{id}/repositories"),
    );
    expect(api.bodies.at(-1)).toEqual({
      kind: "existing",
      directory: "scratch",
      url: "https://github.com/acme/scratch.git",
      pull: true,
      push: true,
    });
    await waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe("scratch is on the list now."),
    );
  });

  it("sends the chosen identity and the cleared boxes", async () => {
    await mount({ repositories: [] }, listing([checkout("scratch")]));
    await fireEvent.click(
      section().getByRole("button", { name: "Manage scratch" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Manage scratch",
    });
    await fireEvent.change(within(dialog).getByLabelText("Git identity"), {
      target: { value: "2" },
    });
    await fireEvent.click(within(dialog).getByLabelText("Push"));
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Manage this" }),
    );
    await waitFor(() => expect(api.bodies.length).toBeGreaterThan(0));
    expect(api.bodies.at(-1)).toMatchObject({
      identity_id: 2,
      pull: true,
      push: false,
    });
  });

  it("shows the refusal when the remote changed, and keeps the dialog open", async () => {
    await mount({ repositories: [] }, listing([checkout("scratch")]));
    api.refuse.set("POST /api/workspaces/{id}/repositories", {
      status: 409,
      message:
        "the remote of scratch is now https://github.com/new/scratch.git, not https://github.com/acme/scratch.git",
    });
    await fireEvent.click(
      section().getByRole("button", { name: "Manage scratch" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Manage scratch",
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Manage this" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      /now https:\/\/github.com\/new\/scratch.git, not https:\/\/github.com\/acme\/scratch.git/,
    );
  });
});

describe("removing and cloning again", () => {
  it("says the files stay, then removes the entry from the list only", async () => {
    await mount();
    await fireEvent.click(
      section().getByRole("button", { name: "Remove api from the list" }),
    );
    const dialog = await screen
      .findByRole("alertdialog")
      .catch(() => screen.findByRole("dialog"));
    expect(dialog).toHaveTextContent(
      /the folder and its files stay in the workspace/,
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Remove from the list" }),
    );
    await waitFor(() =>
      expect(api.calls).toContain(
        "DELETE /api/workspaces/{id}/repositories/{directory}",
      ),
    );
    await waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe(
        "Removed api from the list. Its files stay in the workspace.",
      ),
    );
  });

  it("clones an entry that is not on the disk again", async () => {
    await mount({}, listing([], { missing: ["api"] }));
    expect(section().getByText("Not on the disk")).toBeInTheDocument();
    await fireEvent.click(
      section().getByRole("button", { name: "Clone api again" }),
    );
    await waitFor(() =>
      expect(api.calls).toContain(
        "POST /api/workspaces/{id}/repositories/{directory}/clone",
      ),
    );
    await waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe("Cloning api again."),
    );
  });

  it("shows the refusal of a removal as an alert", async () => {
    await mount();
    api.refuse.set("DELETE /api/workspaces/{id}/repositories/{directory}", {
      status: 409,
      message: "the workspace is busy",
    });
    await fireEvent.click(
      section().getByRole("button", { name: "Remove api from the list" }),
    );
    const dialog = await screen
      .findByRole("alertdialog")
      .catch(() => screen.findByRole("dialog"));
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Remove from the list" }),
    );
    expect(await section().findByRole("alert")).toHaveTextContent(
      "The workspace is busy.",
    );
  });
});

describe("Add repository", () => {
  it("clones a new repository and says so", async () => {
    await mount();
    await fireEvent.click(
      section().getByRole("button", { name: "Add repository" }),
    );
    const dialog = await screen.findByRole("dialog");
    await fireEvent.input(
      within(dialog).getByLabelText("Git repository (HTTPS)"),
      {
        target: { value: "https://github.com/acme/web.git" },
      },
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Clone" }),
    );
    await waitFor(() =>
      expect(api.bodies.at(-1)).toMatchObject({
        kind: "clone",
        url: "https://github.com/acme/web.git",
      }),
    );
    await waitFor(() =>
      expect(toasts.items.at(-1)?.message).toMatch(/^Cloning /),
    );
  });

  it("refuses a repository that is on the list and a directory that is on the disk", async () => {
    await mount(
      {},
      listing([checkout("api", { entry: own }), checkout("web")]),
    );
    await fireEvent.click(
      section().getByRole("button", { name: "Add repository" }),
    );
    const dialog = await screen.findByRole("dialog");
    await fireEvent.input(
      within(dialog).getByLabelText("Git repository (HTTPS)"),
      {
        target: { value: "https://github.com/acme/api" },
      },
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Clone" }),
    );
    expect(
      await within(dialog).findByText(/already on the list, as api/),
    ).toBeInTheDocument();
    await fireEvent.input(
      within(dialog).getByLabelText("Git repository (HTTPS)"),
      {
        target: { value: "https://github.com/acme/web.git" },
      },
    );
    expect(
      await within(dialog).findByText(/web already exists in the workspace/),
    ).toBeInTheDocument();
    expect(api.bodies).toHaveLength(0);
  });

  it("is off while the workspace is stopped, with the reason", async () => {
    await mount(
      { status: "stopped" },
      listing([checkout("api", { entry: own })], { running: false }),
    );
    expect(
      section().getByRole("button", { name: "Add repository" }),
    ).toBeDisabled();
    expect(
      section().getByText("Start the workspace to add a repository."),
    ).toBeInTheDocument();
  });
});

describe("states without a live listing", () => {
  it("says the last listing is old on a stopped workspace", async () => {
    await mount(
      { status: "stopped" },
      listing([checkout("api", { entry: own })], { running: false }),
    );
    expect(
      section().getByText(
        /The workspace is stopped. This is the last listing./,
      ),
    ).toBeInTheDocument();
  });

  it("shows the list's entries unchecked when a stopped workspace was never listed", async () => {
    await mount(
      { status: "stopped" },
      listing([], { running: false, checked_at: null }),
    );
    expect(
      section().getByText("Start the workspace to see its checkouts."),
    ).toBeInTheDocument();
    expect(section().getByText("api")).toBeInTheDocument();
    expect(section().getByText("On the list")).toBeInTheDocument();
  });

  it("says why the guest could not be read", async () => {
    await mount({}, listing([], { problem: "the guest did not answer" }));
    expect(section().getByRole("alert")).toHaveTextContent(
      /the guest did not answer/,
    );
  });

  it("says Puddle could not list when the service refuses", async () => {
    const ws = workspace("dev", { status: "running", repositories: [own] });
    api.list = [ws];
    workspaces.list = [ws];
    api.refuse.set("GET /api/workspaces/{id}/checkouts", {
      status: 500,
      message: "broke",
    });
    render(RepositoriesSection, { workspace: ws });
    expect(await section().findByRole("alert")).toHaveTextContent(
      /Puddle could not list the checkouts/,
    );
  });

  it("reads the listing again when the list changes", async () => {
    await mount();
    api.checkouts["dev"] = listing([
      checkout("api", { entry: own }),
      checkout("new"),
    ]);
    h.source.emit({ type: "workspace_repositories_changed", workspace: "dev" });
    expect(await section().findByText("new")).toBeInTheDocument();
  });

  it("shows the clone in progress", async () => {
    await mount({ busy: "cloning" });
    expect(
      section().getByText(/Stop and delete wait until it ends/),
    ).toBeInTheDocument();
    expect(
      section().getByRole("button", { name: "Add repository" }),
    ).toBeDisabled();
  });
});

describe("identities and empty states", () => {
  it("offers the default identity in the dialog when none covers the address", async () => {
    await mount(
      { repositories: [] },
      listing([
        checkout("lab", { remote_url: "https://gitlab.com/x/lab.git" }),
      ]),
    );
    await fireEvent.click(
      section().getByRole("button", { name: "Manage lab" }),
    );
    const dialog = await screen.findByRole("dialog", { name: "Manage lab" });
    expect(
      within(dialog).getByRole("option", { name: "Let Puddle choose (Work)" }),
    ).toBeInTheDocument();
  });

  it("points to the Git tab when no identity covers a listed repository", async () => {
    idApi.git = { dev: { ids: [], repos: [], push: true, pull: false } };
    await mount();
    expect(
      await section().findByRole("link", { name: "open the Git tab" }),
    ).toHaveAttribute("href", "/workspaces/dev/git");
  });

  it("says why Clone again is off on a stopped workspace", async () => {
    await mount(
      { status: "stopped" },
      listing([], { missing: ["api"], running: false }),
    );
    expect(
      section().getByRole("button", { name: "Clone api again" }),
    ).toBeDisabled();
    expect(
      section().getByText("Start the workspace to clone it again."),
    ).toBeInTheDocument();
  });

  it("says nothing is listed when the disk and the list are both empty", async () => {
    await mount({ repositories: [] }, listing([]));
    expect(
      section().getByText("No checkout is on the disk and none is listed."),
    ).toBeInTheDocument();
  });

  it("says no repository is listed for a stopped workspace that was never read", async () => {
    await mount(
      { status: "stopped", repositories: [] },
      listing([], { running: false, checked_at: null }),
    );
    expect(
      section().getByText("No repository is listed for this workspace."),
    ).toBeInTheDocument();
  });
});
