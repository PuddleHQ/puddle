// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { workspace } from "#lib/testing/fake-workspaces.ts";
import {
  credential,
  ghSource,
  identity,
} from "#lib/testing/fake-identities.ts";
import { FakeRepos, repoSource, repoView } from "#lib/testing/fake-repos.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import type { ActionResult } from "#lib/stores/workspaces.svelte.ts";
import type { Identity } from "#lib/identities/model.ts";
import type { Workspace } from "#lib/workspaces/model.ts";
import CreateWorkspaceDialog from "./CreateWorkspaceDialog.svelte";

// The limit is 100 (pinned in repo-rows.test.ts); a form with that many rows takes seconds to
// build in jsdom, so the dialog's use of the limit is tested with a small one.
const h = vi.hoisted(() => ({
  repos: undefined as unknown,
  rowLimit: undefined as number | undefined,
}));

vi.mock("#lib/stores/repos.svelte.ts", async (original) => {
  const mod = await original<typeof import("#lib/stores/repos.svelte.ts")>();
  return {
    ...mod,
    RepoLists: class extends mod.RepoLists {
      constructor() {
        super({ api: h.repos as never });
      }
    },
  };
});

vi.mock("#lib/workspaces/repo-rows.ts", async (original) => {
  const mod = await original<typeof import("#lib/workspaces/repo-rows.ts")>();
  return {
    ...mod,
    get MAX_REPOSITORIES() {
      return h.rowLimit ?? mod.MAX_REPOSITORIES;
    },
  };
});

vi.mock("$app/navigation", () => ({ goto: vi.fn() }));

afterEach(() => {
  cleanup();
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});

function mount(
  result: ActionResult<Workspace> | (() => Promise<ActionResult<Workspace>>) = {
    ok: true,
    value: workspace("made", { busy: "creating", status: "created" }),
  },
) {
  const create = vi.fn(async (_body: unknown) =>
    typeof result === "function" ? result() : result,
  );
  const onCreated = vi.fn();
  render(CreateWorkspaceDialog, {
    props: { open: true, store: { create }, onCreated },
  } as never);
  return { create, onCreated };
}

const field = (label: string) =>
  screen.getByLabelText(label) as HTMLInputElement;
const submit = () =>
  screen.getByRole("button", { name: /Create workspace|Creating/ });
const type = (label: string, value: string) =>
  fireEvent.input(field(label), { target: { value } });

describe("CreateWorkspaceDialog", () => {
  it("is a dialog with the repository first", async () => {
    mount();
    const dialog = await screen.findByRole("dialog", { name: "New workspace" });
    const first = dialog.querySelector("input");
    expect(first).toBe(screen.getByTestId("source-address"));
    expect(first).toBeChecked();
    expect(dialog.querySelectorAll("input")[2]).toBe(
      field("Git repository (HTTPS)"),
    );
  });

  it("puts focus on the address when it opens, not on the source choice", async () => {
    mount();
    await screen.findByRole("dialog", { name: "New workspace" });
    await vi.waitFor(() =>
      expect(document.activeElement).toBe(field("Git repository (HTTPS)")),
    );
  });

  it("starts from a folder on this computer: only the folder is needed, and the origin fills in the address", async () => {
    const { create, onCreated } = mount();
    await screen.findByRole("dialog");
    await fireEvent.click(screen.getByTestId("source-folder"));
    expect(screen.queryByLabelText("Git repository (HTTPS)")).toBeNull();
    expect(
      screen.getByText(
        /Uncommitted changes, untracked files such as .env, and hooks/,
      ),
    ).toBeInTheDocument();
    await type("Folder of the checkout", "/home/you/Shop/");
    expect(field("Name").value).toBe("shop");
    expect(field("Directory").placeholder).toBe("Shop");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(onCreated).toHaveBeenCalled());
    expect(create).toHaveBeenCalledWith({
      name: "shop",
      repositories: [{ url: "", local_path: "/home/you/Shop/" }],
    });
  });

  it("checks the folder before sending, and puts a refusal about the checkout on it", async () => {
    const { create } = mount({
      ok: false,
      message:
        "repository 1 (the checkout /home/you/shop): it has no origin remote",
    });
    await screen.findByRole("dialog");
    await fireEvent.click(screen.getByTestId("source-folder"));
    await type("Folder of the checkout", "shop");
    await type("Name", "shop");
    await fireEvent.click(submit());
    expect(field("Folder of the checkout")).toHaveFocus();
    expect(screen.getByRole("alert")).toHaveTextContent("Give the full path");
    expect(create).not.toHaveBeenCalled();
    await type("Folder of the checkout", "/home/you/shop");
    await fireEvent.click(submit());
    await vi.waitFor(() =>
      expect(field("Folder of the checkout")).toHaveAttribute(
        "aria-invalid",
        "true",
      ),
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "it has no origin remote",
    );
    // Back to an address: the folder is not sent.
    await fireEvent.click(screen.getByTestId("source-address"));
    await type("Git repository (HTTPS)", "https://github.com/acme/shop.git");
    await fireEvent.click(submit());
    expect(create).toHaveBeenLastCalledWith({
      name: "shop",
      repositories: [{ url: "https://github.com/acme/shop.git" }],
    });
  });

  it("names the workspace after the repository until a name is typed", async () => {
    mount();
    await screen.findByRole("dialog");
    await type(
      "Git repository (HTTPS)",
      "https://github.com/acme/Ledger_Service.git",
    );
    expect(field("Name").value).toBe("ledger-service");
    await type("Name", "mine");
    await type("Git repository (HTTPS)", "https://github.com/acme/other.git");
    expect(field("Name").value).toBe("mine");
  });

  it("sends what was typed, leaving out the image and memory when blank", async () => {
    const { create, onCreated } = mount();
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", " https://github.com/acme/web.git ");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(onCreated).toHaveBeenCalled());
    expect(create).toHaveBeenCalledWith({
      name: "web",
      repositories: [{ url: "https://github.com/acme/web.git" }],
    });
  });

  it("sends the image and the memory in MiB when given", async () => {
    const { create } = mount();
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(screen.getByText("Start folder, image and memory"));
    await type("Image", " ghcr.io/acme/dev:1 ");
    await type("Memory (GiB)", "3");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create).toHaveBeenCalledWith({
      name: "web",
      repositories: [{ url: "https://github.com/acme/web.git" }],
      image: "ghcr.io/acme/dev:1",
      memory_mib: 3072,
    });
  });

  it("checks the fields first and puts focus on the first bad one", async () => {
    const { create } = mount();
    await screen.findByRole("dialog");
    await fireEvent.click(submit());
    expect(screen.getAllByRole("alert")).toHaveLength(2);
    expect(field("Git repository (HTTPS)")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    expect(field("Git repository (HTTPS)")).toHaveFocus();
    expect(create).not.toHaveBeenCalled();
    await type("Git repository (HTTPS)", "git@github.com:acme/web.git");
    await type("Name", "web");
    await fireEvent.click(submit());
    expect(screen.getByRole("alert")).toHaveTextContent(
      "SSH remotes are not supported yet; use the repository's HTTPS URL instead.",
    );
  });

  it("checks the memory field too", async () => {
    const { create } = mount();
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(screen.getByText("Start folder, image and memory"));
    await type("Memory (GiB)", "plenty");
    await fireEvent.click(submit());
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Enter a number of GiB.",
    );
    expect(field("Memory (GiB)")).toHaveFocus();
    expect(create).not.toHaveBeenCalled();
  });

  it.each([
    ["name", "Name", "a workspace named web already exists"],
    ["repo_url", "Git repository (HTTPS)", "use an https:// URL"],
    ["image", "Image", "bad image"],
    ["memory_mib", "Memory (GiB)", "memory is too small"],
  ] as const)(
    "shows a refusal about %s at that field",
    async (fieldName, label, message) => {
      mount({ ok: false, message, field: fieldName });
      await screen.findByRole("dialog");
      await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
      await fireEvent.click(screen.getByText("Start folder, image and memory"));
      await fireEvent.click(submit());
      await vi.waitFor(() =>
        expect(screen.getByRole("alert")).toHaveTextContent(message),
      );
      expect(field(label)).toHaveAttribute("aria-invalid", "true");
      expect(screen.getByRole("dialog")).toBeInTheDocument();
    },
  );

  it("shows any other refusal for the whole form, and keeps what was typed", async () => {
    mount({ ok: false, message: "Puddle's service isn't answering." });
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(submit());
    await vi.waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "Puddle's service isn't answering.",
      ),
    );
    expect(field("Git repository (HTTPS)").value).toBe(
      "https://github.com/acme/web.git",
    );
  });

  it("can't be submitted twice", async () => {
    let finish!: (r: ActionResult<Workspace>) => void;
    const { create } = mount(
      () => new Promise((resolve) => (finish = resolve)),
    );
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(submit());
    expect(screen.getByRole("button", { name: "Creating…" })).toBeDisabled();
    await fireEvent.submit(screen.getByRole("dialog").querySelector("form")!);
    expect(create).toHaveBeenCalledTimes(1);
    finish({ ok: true, value: workspace("web") });
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("forgets what was typed when it is cancelled", async () => {
    mount();
    await screen.findByRole("dialog");
    await type("Name", "half-done");
    await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });
  it("warns with the identity it got when none covers the repository, and offers the Git tab", async () => {
    const { goto } = await import("$app/navigation");
    const warning =
      "No identity covers github.com/acme, so this workspace got your default identity, Personal.";
    mount({
      ok: true,
      value: workspace("made", {
        busy: "creating",
        status: "created",
        identities: [{ identity: "Personal", basis: "default", warning }],
      }),
    });
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(toasts.items).toHaveLength(1));
    const toast = toasts.items[0]!;
    expect(toast.message).toBe(warning);
    expect(toast.tone).toBe("info");
    expect(toast.action?.label).toBe("Open Git tab");
    await toast.action?.run();
    expect(goto).toHaveBeenCalledWith("/workspaces/made/git");
  });

  it("says nothing extra when an identity covers the repository or the answer has no identity", async () => {
    const covered = mount({
      ok: true,
      value: workspace("made", {
        identities: [{ identity: "Work", basis: "covers", warning: null }],
      }),
    });
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(covered.onCreated).toHaveBeenCalled());
    expect(toasts.items).toHaveLength(0);
  });
});

describe("CreateWorkspaceDialog identities and repositories", () => {
  const work = identity(1, {
    label: "Work",
    credentials: [
      credential({
        source: ghSource("tijs-work"),
        owners: ["acme"],
        rest: false,
      }),
    ],
  });
  const personal = identity(2, {
    label: "Personal",
    is_default: false,
    credentials: [credential({ source: ghSource("tijs-demo"), rest: true })],
  });
  const ensureLoaded = vi.fn(async () => {});

  beforeEach(() => {
    const repos = new FakeRepos();
    repos.repos = [repoView("acme", "web", { identities: [2, 1] })];
    repos.sources = [repoSource({ repo_count: 1 })];
    h.repos = repos;
    ensureLoaded.mockClear();
    for (const t of [...toasts.items]) toasts.dismiss(t.id);
  });

  function open(
    prefill: { url: string; identity: number | null } | null = null,
    result: ActionResult<Workspace> = {
      ok: true,
      value: workspace("web", { busy: "creating", status: "created" }),
    },
    store: {
      identities: Identity[];
      status?: "loading" | "ready" | "failed";
    } = { identities: [work, personal] },
  ) {
    const create = vi.fn(async (_body: unknown) => result);
    const onCreated = vi.fn();
    render(CreateWorkspaceDialog, {
      props: {
        open: true,
        store: { create },
        identities: { ...store, ensureLoaded },
        prefill,
        onCreated,
      },
    } as never);
    return { create, onCreated };
  }

  const identityBox = () =>
    screen.getByRole("combobox", { name: "Git identity" });

  it("reads the identities when it opens", async () => {
    open();
    await screen.findByRole("dialog");
    expect(ensureLoaded).toHaveBeenCalled();
  });

  it("offers the identities that cover a typed address and chooses the one that covers its owner", async () => {
    open();
    await screen.findByRole("dialog");
    expect(identityBox()).toHaveDisplayValue("Let Puddle choose");
    expect(screen.queryByRole("group", { name: "Cover this repository" })).toBe(
      null,
    );
    await type("Git repository (HTTPS)", "https://github.com/acme/web");
    expect(identityBox()).toHaveDisplayValue("Work");
    const covering = screen.getByRole("group", {
      name: "Cover this repository",
    });
    expect(covering).toHaveTextContent("Work");
    expect(covering).toHaveTextContent("Personal");
    await type("Git repository (HTTPS)", "https://github.com/someone/else");
    expect(identityBox()).toHaveDisplayValue("Personal");
    // An identity that does not cover it is still on offer, under its own heading.
    await type("Git repository (HTTPS)", "https://gitlab.com/acme/web");
    expect(identityBox()).toHaveDisplayValue("Let Puddle choose");
    expect(
      screen.getByRole("group", { name: "Other identities" }),
    ).toBeVisible();
  });

  it("shows which identity the repository will get as the address and the pick change", async () => {
    open();
    await screen.findByRole("dialog");
    expect(screen.queryByText(/Uses |No identity covers/)).toBeNull();
    await type("Git repository (HTTPS)", "https://github.com/acme/web");
    expect(identityBox()).toHaveAccessibleDescription(
      "Uses Work: it covers acme on github.com.",
    );
    await type("Git repository (HTTPS)", "https://github.com/someone/else");
    expect(identityBox()).toHaveAccessibleDescription(
      "Uses Personal: it covers someone on github.com.",
    );
    // Nothing covers it: the default, with the consequence spelled out.
    await type("Git repository (HTTPS)", "https://gitlab.com/acme/web");
    expect(identityBox()).toHaveAccessibleDescription(
      /No identity covers acme on gitlab\.com, so Puddle uses the default, Work\. .*without a credential/,
    );
    await fireEvent.change(identityBox(), { target: { value: "2" } });
    expect(identityBox()).toHaveAccessibleDescription(
      /Uses Personal, which has no credential for acme on gitlab\.com/,
    );
    await fireEvent.change(identityBox(), { target: { value: "" } });
    expect(identityBox()).toHaveAccessibleDescription(/the default, Work/);
  });

  it("says a workspace gets no identity when there are none, and waits for the identities", async () => {
    open(null, undefined, { identities: [] });
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web");
    expect(identityBox()).toHaveAccessibleDescription(
      /no commit author and no credential/,
    );
    cleanup();
    open(null, undefined, { identities: [work], status: "loading" });
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web");
    expect(identityBox()).not.toHaveAccessibleDescription();
  });

  it("lets a later row see the identities the earlier rows bring", async () => {
    const both = identity(3, {
      label: "Both",
      is_default: false,
      credentials: [credential({ rest: true })],
    });
    open(null, undefined, { identities: [work, personal, both] });
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web");
    await fireEvent.click(
      screen.getByRole("button", { name: "Add another repository" }),
    );
    const second = () =>
      screen.getAllByRole("combobox", { name: "Git identity" })[1]!;
    await fireEvent.input(
      screen.getAllByLabelText("Git repository (HTTPS)")[1]!,
      { target: { value: "https://github.com/someone/else" } },
    );
    // Personal and Both both cover "someone"; neither is on the list yet, so the first wins.
    expect(second()).toHaveAccessibleDescription(
      "Uses Personal: it covers someone on github.com.",
    );
    // Told to choose, the host prefers one already on the list: once the first repository takes
    // Both, the second gets it too.
    await fireEvent.change(second(), { target: { value: "" } });
    expect(second()).toHaveAccessibleDescription(
      "Uses Personal: it covers someone on github.com.",
    );
    await fireEvent.change(
      screen.getAllByRole("combobox", { name: "Git identity" })[0]!,
      { target: { value: "3" } },
    );
    expect(second()).toHaveAccessibleDescription(
      "Uses Both: it covers someone on github.com.",
    );
  });

  it('fills the repository from "Create a workspace for this" and chooses the identity that listed it', async () => {
    open({ url: "https://github.com/acme/web", identity: 2 });
    await screen.findByRole("dialog");
    expect(field("Git repository (HTTPS)").value).toBe(
      "https://github.com/acme/web",
    );
    expect(field("Name").value).toBe("web");
    expect(identityBox()).toHaveDisplayValue(
      /Personal \(listed this repository\)/,
    );
  });

  it("sends the identity the row shows, and one picked by hand in its place", async () => {
    const { create } = open({
      url: "https://github.com/acme/web",
      identity: 2,
    });
    await screen.findByRole("dialog");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalledTimes(1));
    expect(create.mock.calls[0]?.[0]).toMatchObject({
      repositories: [{ url: "https://github.com/acme/web", identity_id: 2 }],
    });
  });

  it("sends an identity picked by hand", async () => {
    const { create } = open();
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://gitlab.com/acme/web");
    await fireEvent.change(identityBox(), { target: { value: "1" } });
    expect(identityBox()).toHaveDisplayValue("Work");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create.mock.calls[0]?.[0]).toMatchObject({
      repositories: [{ identity_id: 1 }],
    });
  });

  it("sends no identity when Puddle is told to choose", async () => {
    const { create } = open({
      url: "https://github.com/acme/web",
      identity: 1,
    });
    await screen.findByRole("dialog");
    await fireEvent.change(identityBox(), { target: { value: "" } });
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create.mock.calls[0]?.[0]).toEqual({
      name: "web",
      repositories: [{ url: "https://github.com/acme/web" }],
    });
  });

  it("takes the repository and identity of a pick from the list, and moves focus to the name", async () => {
    open();
    await screen.findByRole("dialog");
    await fireEvent.click(screen.getByText("Choose from your repositories"));
    await fireEvent.click(
      await screen.findByRole("button", { name: /acme\/web/ }),
    );
    expect(field("Git repository (HTTPS)").value).toBe(
      "https://github.com/acme/web",
    );
    expect(field("Name")).toHaveFocus();
    // The list names Personal first, so it listed the repository first.
    expect(identityBox()).toHaveDisplayValue(
      /Personal \(listed this repository\)/,
    );
  });

  it("forgets the identity that listed it when the address is typed over", async () => {
    open({ url: "https://github.com/acme/web", identity: 2 });
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/api");
    expect(identityBox()).toHaveDisplayValue("Work");
    expect(screen.queryByText(/listed this repository/)).toBeNull();
  });

  it("warns about an Azure DevOps project with a space in the address", async () => {
    open();
    await screen.findByRole("dialog");
    await type(
      "Git repository (HTTPS)",
      "https://dev.azure.com/contoso/Shop%20Floor/_git/scanner",
    );
    expect(screen.getByTestId("table-cannot-hold")).toHaveTextContent(
      "Only push to listed repos",
    );
  });
});

describe("CreateWorkspaceDialog with several repositories", () => {
  const url = (n: number) =>
    within(row(n)).getByLabelText("Git repository (HTTPS)") as HTMLInputElement;
  const row = (n: number) =>
    screen.getByRole("group", { name: `Repository ${n}` });
  const part = (n: number, label: string) =>
    within(row(n)).getByLabelText(label) as HTMLInputElement;
  const typeIn = (n: number, label: string, value: string) =>
    fireEvent.input(part(n, label), { target: { value } });
  const add = () =>
    screen.getByRole("button", {
      name: /Add (another|a) repository/,
    });

  function open(
    result: ActionResult<Workspace> = {
      ok: true,
      value: workspace("api", { busy: "creating", status: "created" }),
    },
  ) {
    const create = vi.fn(async (_body: unknown) => result);
    render(CreateWorkspaceDialog, {
      props: {
        open: true,
        store: { create },
        identities: { identities: [], ensureLoaded: async () => {} },
      },
    } as never);
    return { create };
  }

  beforeEach(() => {
    h.repos = new FakeRepos();
    h.rowLimit = undefined;
    for (const t of [...toasts.items]) toasts.dismiss(t.id);
  });

  it("adds a row and puts focus on its address; Remove puts focus on the next row's address, or on Add", async () => {
    open();
    await screen.findByRole("dialog");
    await fireEvent.click(add());
    await vi.waitFor(() => expect(url(2)).toHaveFocus());
    await fireEvent.click(add());
    await vi.waitFor(() => expect(url(3)).toHaveFocus());
    await fireEvent.click(
      screen.getByRole("button", { name: "Remove repository 2" }),
    );
    // Row 3 is row 2 now, and it has the focus.
    await vi.waitFor(() => expect(url(2)).toHaveFocus());
    expect(screen.queryByRole("group", { name: "Repository 3" })).toBeNull();
    await fireEvent.click(
      screen.getByRole("button", { name: "Remove repository 2" }),
    );
    await vi.waitFor(() => expect(add()).toHaveFocus());
  });

  it("says an empty list makes an ordinary workspace, and sends an empty list", async () => {
    const { create } = open();
    await screen.findByRole("dialog");
    await fireEvent.click(
      screen.getByRole("button", { name: "Remove repository 1" }),
    );
    expect(
      screen.getByText(/No repository: this makes an ordinary/),
    ).toBeVisible();
    expect(add()).toHaveTextContent("Add a repository");
    await type("Name", "scratch");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create.mock.calls[0]?.[0]).toEqual({
      name: "scratch",
      repositories: [],
    });
  });

  it("names the workspace after the first repository only", async () => {
    open();
    await screen.findByRole("dialog");
    await fireEvent.click(add());
    await fireEvent.input(url(2), {
      target: { value: "https://github.com/acme/second.git" },
    });
    expect(field("Name").value).toBe("");
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/first.git" },
    });
    expect(field("Name").value).toBe("first");
  });

  it("sends each row with its directory and switches, and the start folder", async () => {
    const { create } = open();
    await screen.findByRole("dialog");
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await fireEvent.click(add());
    await fireEvent.input(url(2), {
      target: { value: "https://github.com/acme/web.git" },
    });
    await typeIn(2, "Directory", " frontend ");
    await fireEvent.click(
      within(row(2)).getByRole("checkbox", { name: "Push" }),
    );
    await fireEvent.click(screen.getByText("Start folder, image and memory"));
    await type("Start folder", " api/src ");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create.mock.calls[0]?.[0]).toEqual({
      name: "api",
      repositories: [
        { url: "https://github.com/acme/api.git" },
        {
          url: "https://github.com/acme/web.git",
          directory: "frontend",
          push: false,
        },
      ],
      start_folder: "api/src",
    });
  });

  it("sends a row whose Pull switch was cleared", async () => {
    const { create } = open();
    await screen.findByRole("dialog");
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await fireEvent.click(
      within(row(1)).getByRole("checkbox", { name: "Pull" }),
    );
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create.mock.calls[0]?.[0]).toEqual({
      name: "api",
      repositories: [{ url: "https://github.com/acme/api.git", pull: false }],
    });
  });

  it("puts focus on the name when only the name is wrong", async () => {
    const { create } = open();
    await screen.findByRole("dialog");
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await type("Name", "");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(field("Name")).toHaveFocus());
    expect(field("Name")).toHaveAttribute("aria-invalid", "true");
    expect(create).not.toHaveBeenCalled();
  });

  it("takes a pick from the list in a later row and moves focus to that row's directory", async () => {
    const repos = new FakeRepos();
    repos.repos = [repoView("acme", "web")];
    repos.sources = [repoSource({ repo_count: 1 })];
    h.repos = repos;
    open();
    await screen.findByRole("dialog");
    await fireEvent.click(add());
    await vi.waitFor(() => expect(url(2)).toHaveFocus());
    await fireEvent.click(
      within(row(2)).getByText("Choose from your repositories"),
    );
    await fireEvent.click(
      await screen.findByRole("button", { name: /acme\/web/ }),
    );
    expect(url(2).value).toBe("https://github.com/acme/web");
    await vi.waitFor(() => expect(part(2, "Directory")).toHaveFocus());
  });

  it("flags two rows that share a directory as you type, naming a free one, and clears it when one changes", async () => {
    const { create } = open();
    await screen.findByRole("dialog");
    await fireEvent.click(add());
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await fireEvent.input(url(2), {
      target: { value: "https://gitlab.com/other/api.git" },
    });
    for (const n of [1, 2]) {
      expect(part(n, "Directory")).toHaveAttribute("aria-invalid", "true");
    }
    expect(within(row(2)).getByRole("alert")).toHaveTextContent(
      "Repository 1, the earlier one, also goes into api. Use another directory, such as api-2.",
    );
    await fireEvent.input(url(1), { target: { value: "" } });
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await typeIn(2, "Directory", "api-2");
    expect(part(1, "Directory")).not.toHaveAttribute("aria-invalid");
    expect(part(2, "Directory")).not.toHaveAttribute("aria-invalid");
    expect(create).not.toHaveBeenCalled();
  });

  it("refuses a submit with a clash, a bad address or a bad directory, and focuses the first", async () => {
    const { create } = open();
    await screen.findByRole("dialog");
    await fireEvent.click(add());
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await fireEvent.input(url(2), {
      target: { value: "https://gitlab.com/other/api.git" },
    });
    await fireEvent.click(submit());
    expect(part(1, "Directory")).toHaveFocus();
    expect(create).not.toHaveBeenCalled();
    await typeIn(1, "Directory", "-bad");
    await fireEvent.click(submit());
    expect(within(row(1)).getAllByRole("alert")[0]).toHaveTextContent(
      "Don't start with a dot or a hyphen.",
    );
    await typeIn(1, "Directory", "");
    await typeIn(2, "Directory", "other");
    await fireEvent.input(url(2), { target: { value: "ftp://x/y" } });
    await fireEvent.click(submit());
    expect(url(2)).toHaveFocus();
    expect(within(row(2)).getByRole("alert")).toHaveTextContent("https://");
  });

  it("refuses the same repository in two spellings", async () => {
    open();
    await screen.findByRole("dialog");
    await fireEvent.click(add());
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/Acme/API.git" },
    });
    await fireEvent.input(url(2), {
      target: { value: "https://github.com/acme/api/" },
    });
    await typeIn(2, "Directory", "other");
    await fireEvent.click(submit());
    expect(within(row(2)).getByRole("alert")).toHaveTextContent(
      "This is the same repository as repository 1. List it once.",
    );
    expect(url(2)).toHaveFocus();
  });

  it("checks the start folder as you type and opens the options when it is refused", async () => {
    const { create } = open();
    await screen.findByRole("dialog");
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await fireEvent.click(screen.getByText("Start folder, image and memory"));
    await type("Start folder", "../escape");
    expect(field("Start folder")).toHaveAttribute("aria-invalid", "true");
    await fireEvent.click(screen.getByText("Start folder, image and memory"));
    await fireEvent.click(submit());
    expect(field("Start folder")).toHaveFocus();
    expect(create).not.toHaveBeenCalled();
  });

  it("puts a refusal that names a repository at that row", async () => {
    open({
      ok: false,
      message:
        "repositories 1 (https://github.com/acme/api.git) and 2 (https://gitlab.com/o/api.git) both go into api; give one of them another directory, such as api-2",
      field: "repo_url",
    });
    await screen.findByRole("dialog");
    await fireEvent.click(add());
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await fireEvent.input(url(2), {
      target: { value: "https://gitlab.com/o/api.git" },
    });
    await typeIn(1, "Directory", "one");
    await typeIn(2, "Directory", "two");
    await fireEvent.click(submit());
    await vi.waitFor(() =>
      expect(within(row(2)).getByRole("alert")).toHaveTextContent(
        "both go into api",
      ),
    );
    expect(part(2, "Directory")).toHaveFocus();
  });

  it("puts a refusal about an address at the form when it names no row of several", async () => {
    open({ ok: false, message: "the host is unreachable", field: "repo_url" });
    await screen.findByRole("dialog");
    await fireEvent.click(add());
    await fireEvent.input(url(2), {
      target: { value: "https://github.com/acme/web.git" },
    });
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await fireEvent.click(submit());
    await vi.waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("unreachable"),
    );
  });

  it("puts a refusal about the start folder at its field", async () => {
    open({
      ok: false,
      message: "the start folder is odd",
      field: "start_folder",
    });
    await screen.findByRole("dialog");
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/acme/api.git" },
    });
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(field("Start folder")).toHaveFocus());
  });

  it("gives one warning for the repositories no identity covers, and counts the rest", async () => {
    const one = "No identity covers github.com/a, so it got Personal.";
    const { create } = open({
      ok: true,
      value: workspace("made", {
        identities: [
          { identity: "Personal", basis: "default", warning: one },
          { identity: "Work", basis: "covers", warning: null },
          { identity: "Personal", basis: "default", warning: "x" },
          { identity: "Personal", basis: "default", warning: "y" },
        ],
      }),
    });
    await screen.findByRole("dialog");
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/a/api.git" },
    });
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    await vi.waitFor(() => expect(toasts.items).toHaveLength(1));
    expect(toasts.items[0]?.message).toBe(
      `${one} 2 more repositories have the same trouble.`,
    );
  });

  it("says 'repository has' for a single other warning", async () => {
    open({
      ok: true,
      value: workspace("made", {
        identities: [
          { identity: "P", basis: "default", warning: "A." },
          { identity: "P", basis: "default", warning: "B." },
        ],
      }),
    });
    await screen.findByRole("dialog");
    await fireEvent.input(url(1), {
      target: { value: "https://github.com/a/api.git" },
    });
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(toasts.items).toHaveLength(1));
    expect(toasts.items[0]?.message).toBe(
      "A. 1 more repository has the same trouble.",
    );
  });

  it("stops adding at the row limit", async () => {
    h.rowLimit = 4;
    open();
    await screen.findByRole("dialog");
    const button = add();
    for (let i = 1; i < 4; i++) await fireEvent.click(button);
    expect(
      screen.getAllByRole("group", { name: /^Repository \d+$/ }),
    ).toHaveLength(4);
    expect(button).toBeDisabled();
    await fireEvent.click(button);
    expect(
      screen.getAllByRole("group", { name: /^Repository \d+$/ }),
    ).toHaveLength(4);
  });
});
