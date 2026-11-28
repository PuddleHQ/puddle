// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { workspace } from "#lib/testing/fake-workspaces.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import {
  workspaces,
  type ActionResult,
} from "#lib/stores/workspaces.svelte.ts";
import type { Workspace } from "#lib/workspaces/model.ts";
import { savedDraft } from "#lib/workspaces/draft.ts";
import { session } from "#lib/session/session.svelte.ts";
import CreateWorkspaceForm from "./CreateWorkspaceForm.svelte";

const { h } = await vi.hoisted(
  () => import("./create-workspace-form-harness.ts"),
);

vi.mock("#lib/stores/repos.svelte.ts", async (original) =>
  (await import("./create-workspace-form-harness.ts")).reposMock(
    await original(),
  ),
);
vi.mock("#lib/workspaces/repo-rows.ts", async (original) =>
  (await import("./create-workspace-form-harness.ts")).repoRowsMock(
    await original(),
  ),
);
vi.mock(
  "#lib/desktop/folder-picker.ts",
  async () =>
    (await import("./create-workspace-form-harness.ts")).folderPickerMock,
);
vi.mock("$app/navigation", () => ({ goto: vi.fn() }));

import {
  field,
  installReset,
  mount,
  mountWithLimits,
  settings,
  submit,
  type,
} from "./create-workspace-form-setup.ts";

installReset();

describe("the title", () => {
  const titleLevel = (embedded: boolean) => {
    render(CreateWorkspaceForm, {
      props: { store: { create: vi.fn() }, settings: settings(), embedded },
    } as never);
    return screen.getByRole("heading", { name: "New workspace" }).tagName;
  };

  it("is the page's h1 on its own screen", () => {
    expect(titleLevel(false)).toBe("H1");
  });

  it("is an h2 under another screen's h1", () => {
    expect(titleLevel(true)).toBe("H2");
  });
});

describe("the default image's description", () => {
  function mountWith(description: string | null, image: string | null = null) {
    render(CreateWorkspaceForm, {
      props: {
        store: { create: vi.fn() },
        settings: settings(),
        defaultImage: { image, description, load: vi.fn(async () => {}) },
      },
    } as never);
  }

  it("shows what the default image is under the empty Image field", async () => {
    mountWith("Debian trixie dev container with git");
    const hint = await screen.findByText(
      "Default: Debian trixie dev container with git",
    );
    expect(hint.textContent?.trim()).not.toBe("");
    expect(
      screen.getByLabelText("Image").getAttribute("aria-describedby"),
    ).toBe(hint.id);
  });

  it("shows the default image's name and memory as the placeholders of the empty fields", async () => {
    mountWith(null, "ghcr.io/puddle/dev:3");
    expect(screen.getByLabelText("Image")).toHaveAttribute(
      "placeholder",
      "ghcr.io/puddle/dev:3",
    );
    expect(screen.getByLabelText("Memory (GiB)")).toHaveAttribute(
      "placeholder",
      "8 GiB",
    );
    cleanup();
    mountWith(null);
    expect(screen.getByLabelText("Image")).toHaveAttribute(
      "placeholder",
      "Default development image",
    );
  });

  it("drops the line once an image is typed, and shows none before it is known", async () => {
    mountWith("Debian trixie dev container with git");
    await type("Image", "ghcr.io/acme/dev:1");
    expect(screen.queryByText(/^Default:/)).toBeNull();
    cleanup();
    mountWith(null);
    expect(screen.queryByText(/^Default:/)).toBeNull();
  });
});

describe("CreateWorkspaceForm", () => {
  it("selects the suggested name on focus so typing replaces it, and leaves a typed name alone", async () => {
    mount();
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/shop");
    const name = field("Name");
    expect(name.value).toBe("shop");
    name.focus();
    expect([name.selectionStart, name.selectionEnd]).toEqual([0, 4]);
    await fireEvent.input(name, { target: { value: "mine" } });
    name.setSelectionRange(4, 4);
    name.blur();
    name.focus();
    expect([name.selectionStart, name.selectionEnd]).toEqual([4, 4]);
  });

  it("keeps the suggested name selected through the mouse-up of the focusing click only", async () => {
    mount();
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/shop");
    const name = field("Name");
    expect(await fireEvent.mouseUp(name)).toBe(true);
    name.focus();
    expect(await fireEvent.mouseUp(name)).toBe(false);
    expect(await fireEvent.mouseUp(name)).toBe(true);
  });

  it("is a dialog with the repository first", async () => {
    mount();
    const dialog = await screen.findByRole("region", { name: "New workspace" });
    const first = dialog.querySelector("input");
    expect(first).toBe(screen.getByTestId("source-address"));
    expect(first).toBeChecked();
    expect(dialog.querySelectorAll("input")[2]).toBe(
      field("Git repository (HTTPS)"),
    );
  });

  it("puts focus on the address when it opens, not on the source choice", async () => {
    mount();
    await screen.findByRole("region", { name: "New workspace" });
    await vi.waitFor(() =>
      expect(document.activeElement).toBe(field("Git repository (HTTPS)")),
    );
  });

  it("starts from a folder on this computer: only the folder is needed, and the origin fills in the address", async () => {
    const { create, onCreated } = mount();
    await screen.findByRole("region");
    await fireEvent.click(screen.getByTestId("source-folder"));
    expect(screen.queryByLabelText("Git repository (HTTPS)")).toBeNull();
    expect(
      screen.getByText(
        /Uncommitted changes, untracked files such as .env, and hooks/,
      ),
    ).toBeInTheDocument();
    await type("Folder of the checkout", "/home/you/Shop/");
    expect(field("Name").value).toBe("Shop");
    expect(field("Directory").placeholder).toBe("Shop");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(onCreated).toHaveBeenCalled());
    expect(create).toHaveBeenCalledWith({
      name: "Shop",
      repositories: [{ url: "", local_path: "/home/you/Shop/" }],
    });
  });

  it("offers the folder source to the owner and to a window with nothing to sign in to", async () => {
    session.mode = "local";
    mount();
    await screen.findByRole("region");
    expect(screen.getByTestId("source-folder")).toBeInTheDocument();
    cleanup();
    session.mode = "signed-in";
    session.user = { id: 1, name: "ann", role: "owner" };
    mount();
    await screen.findByRole("region");
    expect(screen.getByTestId("source-folder")).toBeInTheDocument();
  });

  it("hides the folder source from a signed-in user who is not the owner, and still offers the address", async () => {
    session.mode = "signed-in";
    session.user = { id: 2, name: "bob", role: "user" };
    mount();
    await screen.findByRole("region");
    expect(screen.queryByTestId("source-folder")).toBeNull();
    expect(screen.queryByText(/From a folder on this computer/)).toBeNull();
    expect(screen.getByTestId("source-address")).toBeInTheDocument();
  });

  it("offers no folder dialog in a browser", async () => {
    mount();
    await screen.findByRole("region");
    await fireEvent.click(screen.getByTestId("source-folder"));
    expect(screen.queryByTestId("choose-folder")).toBeNull();
  });

  describe("in the desktop app", () => {
    beforeEach(() => {
      h.desktop = true;
    });

    it("fills the folder field from the dialog, and the name follows", async () => {
      h.pick.mockResolvedValue("/home/you/Shop");
      mount();
      await screen.findByRole("region");
      await fireEvent.click(screen.getByTestId("source-folder"));
      await fireEvent.click(
        screen.getByRole("button", { name: "Choose folder" }),
      );
      await vi.waitFor(() =>
        expect(field("Folder of the checkout").value).toBe("/home/you/Shop"),
      );
      expect(field("Name").value).toBe("Shop");
      expect(h.pick).toHaveBeenCalledTimes(1);
    });

    it("leaves the typed path alone when the dialog is cancelled", async () => {
      h.pick.mockResolvedValue(null);
      mount();
      await screen.findByRole("region");
      await fireEvent.click(screen.getByTestId("source-folder"));
      await type("Folder of the checkout", "/home/you/typed");
      await fireEvent.click(screen.getByTestId("choose-folder"));
      await vi.waitFor(() => expect(h.pick).toHaveBeenCalled());
      expect(field("Folder of the checkout").value).toBe("/home/you/typed");
      expect(screen.getByTestId("choose-folder")).toBeEnabled();
    });

    it("says so and keeps the typed path when the dialog cannot open", async () => {
      h.pick.mockRejectedValue(new Error("no dialog"));
      mount();
      await screen.findByRole("region");
      await fireEvent.click(screen.getByTestId("source-folder"));
      await type("Folder of the checkout", "/home/you/typed");
      await fireEvent.click(screen.getByTestId("choose-folder"));
      await vi.waitFor(() =>
        expect(toasts.items.map((t) => t.message)).toContain(
          "The folder dialog could not open. Type the path instead.",
        ),
      );
      expect(field("Folder of the checkout").value).toBe("/home/you/typed");
    });
  });

  it("checks the folder before sending, and puts a refusal about the checkout on it", async () => {
    const { create } = mount({
      ok: false,
      message:
        "repository 1 (the checkout /home/you/shop): it has no origin remote",
      field: "repo_url",
      at: { row: 1, input: "localPath" },
    });
    await screen.findByRole("region");
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

  it("drops the quotes of a copied Windows path before sending it", async () => {
    const { create } = mount();
    await screen.findByRole("region");
    await fireEvent.click(screen.getByTestId("source-folder"));
    await type("Folder of the checkout", '"C:\\Users\\you\\shop"');
    await type("Name", "shop");
    await fireEvent.click(submit());
    expect(create).toHaveBeenCalledWith({
      name: "shop",
      repositories: [{ url: "", local_path: "C:\\Users\\you\\shop" }],
    });
  });

  it("refuses the root of a drive on the folder field", async () => {
    const { create } = mount();
    await screen.findByRole("region");
    await fireEvent.click(screen.getByTestId("source-folder"));
    await type("Folder of the checkout", "C:\\");
    await type("Name", "root");
    await fireEvent.click(submit());
    expect(screen.getByRole("alert")).toHaveTextContent("not the root");
    expect(create).not.toHaveBeenCalled();
  });

  it("forgets a refusal shown on the folder when the source option changes", async () => {
    mount();
    await screen.findByRole("region");
    await fireEvent.click(screen.getByTestId("source-folder"));
    await type("Folder of the checkout", "shop");
    await type("Name", "shop");
    await fireEvent.click(submit());
    expect(screen.getByRole("alert")).toHaveTextContent("Give the full path");
    await fireEvent.click(screen.getByTestId("source-address"));
    await fireEvent.click(screen.getByTestId("source-folder"));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(field("Folder of the checkout")).not.toHaveAttribute(
      "aria-invalid",
      "true",
    );
  });

  it("takes any text as the name: spaces, capitals and other scripts go as typed", async () => {
    const { create } = mount();
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/shop.git");
    await type("Name", "  Shop API \u00fc (prod)  ");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create.mock.calls[0]?.[0]).toMatchObject({
      name: "  Shop API \u00fc (prod)  ",
    });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("checks the name only for empty, too long, a control character and another workspace's name", async () => {
    workspaces.list = [workspace("a", { name: "Web shop" })];
    const { create } = mount();
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/shop.git");
    for (const [text, problem] of [
      ["", "Give the workspace a name."],
      ["x".repeat(61), "Use at most 60 characters."],
      ["web SHOP", 'Another workspace is already called "Web shop".'],
    ] as const) {
      await type("Name", text);
      await fireEvent.click(submit());
      expect(await screen.findByRole("alert")).toHaveTextContent(problem);
      expect(field("Name")).toHaveFocus();
    }
    expect(create).not.toHaveBeenCalled();
    workspaces.list = [];
  });

  it("names the workspace after the repository until a name is typed", async () => {
    mount();
    await screen.findByRole("region");
    await type(
      "Git repository (HTTPS)",
      "https://github.com/acme/Ledger_Service.git",
    );
    expect(field("Name").value).toBe("Ledger_Service");
    await type("Name", "mine");
    await type("Git repository (HTTPS)", "https://github.com/acme/other.git");
    expect(field("Name").value).toBe("mine");
  });

  it("sends what was typed, leaving out the image and memory when blank", async () => {
    const { create, onCreated } = mount();
    await screen.findByRole("region");
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
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
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

  it("has a CPU slider from 1 to the maximum, on the default, that stores the chosen count", async () => {
    const { create, source } = mountWithLimits(
      { max_cpus: 12, host_cpus: 12 },
      4,
    );
    await screen.findByRole("region");
    expect(source.load).toHaveBeenCalledWith(true);
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    const slider = screen.getByLabelText("CPUs") as HTMLInputElement;
    expect([slider.type, slider.min, slider.max, slider.value]).toEqual([
      "range",
      "1",
      "12",
      "4",
    ]);
    expect(slider.getAttribute("aria-valuetext")).toBe("4 CPUs");
    expect(screen.getByText(/This computer has 12\./)).toBeInTheDocument();
    expect(slider.parentElement?.querySelector("output")).toHaveTextContent(
      "4",
    );
    await fireEvent.input(slider, { target: { value: "7" } });
    expect(slider.parentElement?.querySelector("output")).toHaveTextContent(
      "7",
    );
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create).toHaveBeenCalledWith({
      name: "web",
      repositories: [{ url: "https://github.com/acme/web.git" }],
      cpus: 7,
    });
  });

  it("stores the default count when the slider is left alone, and a single CPU at the bottom", async () => {
    const { create } = mountWithLimits({ max_cpus: 2, host_cpus: 2 }, 1);
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create).toHaveBeenCalledWith(expect.objectContaining({ cpus: 1 }));
  });

  it("shows no slider and sends no count before the limits are known", async () => {
    mount();
    await screen.findByRole("region");
    expect(screen.queryByLabelText("CPUs")).toBeNull();
  });

  it("puts the service's refusal of the count at the slider and opens the fold", async () => {
    const { create } = mountWithLimits({ max_cpus: 12, host_cpus: 12 }, 4, {
      ok: false,
      field: "cpus",
      message: "cpus: 12 is more than the most CPUs a workspace may have (8)",
    });
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    const alert = await screen.findByText(/12 is more than the most CPUs/);
    expect(alert).toHaveAttribute("role", "alert");
    const slider = screen.getByLabelText("CPUs");
    expect(slider).toHaveAttribute("aria-invalid", "true");
    await fireEvent.input(slider, { target: { value: "8" } });
    expect(screen.queryByText(/12 is more than the most CPUs/)).toBeNull();
  });

  it("checks the fields first and puts focus on the first bad one", async () => {
    const { create } = mount();
    await screen.findByRole("region");
    await fireEvent.click(submit());
    expect(screen.getAllByRole("alert")).toHaveLength(2);
    expect(field("Git repository (HTTPS)")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    expect(field("Git repository (HTTPS)")).toHaveFocus();
    expect(create).not.toHaveBeenCalled();
    await type("Git repository (HTTPS)", "git@git.example.com:acme/web.git");
    await type("Name", "web");
    await fireEvent.click(submit());
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Puddle reads SSH addresses of GitHub, GitLab, Bitbucket and Azure DevOps. For another host, use the repository's HTTPS URL.",
    );
  });

  it("takes an SSH address of a Git host, shows its HTTPS address and sends that", async () => {
    const { create } = mount();
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "git@github.com:acme/web.git");
    await type("Name", "web");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(
      screen.getByText("Will clone https://github.com/acme/web.git"),
    ).toBeInTheDocument();
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(create).toHaveBeenCalled());
    expect(create.mock.calls[0]?.[0]).toMatchObject({
      repositories: [{ url: "https://github.com/acme/web.git" }],
    });
  });

  describe("the repository field", () => {
    const repo = () => field("Git repository (HTTPS)");

    it.each([
      ["sharkdp/fd", "https://github.com/sharkdp/fd"],
      ["github.com/sharkdp/fd", "https://github.com/sharkdp/fd"],
      ["gitlab.com/acme/web", "https://gitlab.com/acme/web"],
    ])(
      "reads %j as %j, says so under the field and sends that",
      async (typed, url) => {
        const { create } = mount();
        await screen.findByRole("region");
        await type("Git repository (HTTPS)", typed);
        expect(screen.getByText(`Will clone ${url}`)).toBeInTheDocument();
        expect(repo()).toHaveAccessibleDescription(`Will clone ${url}`);
        await fireEvent.click(submit());
        await vi.waitFor(() => expect(create).toHaveBeenCalled());
        expect(create.mock.calls[0]?.[0]).toMatchObject({
          repositories: [{ url }],
        });
      },
    );

    it("says nothing extra for an address that is already the one cloned", async () => {
      mount();
      await screen.findByRole("region");
      await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
      expect(screen.queryByText(/^Will clone/)).toBeNull();
    });

    it("refuses an address with credentials at the keystroke, before Create", async () => {
      const { create } = mount();
      await screen.findByRole("region");
      await type("Git repository (HTTPS)", "https://user:pw@github.com/x/y");
      // No timer, no blur: the message is there as soon as the typing event is handled.
      expect(screen.getByRole("alert")).toHaveTextContent(
        "Remove the user name and password from the URL; Puddle supplies credentials itself.",
      );
      expect(repo()).toHaveAttribute("aria-invalid", "true");
      expect(screen.queryByText(/^Will clone/)).toBeNull();
      expect(create).not.toHaveBeenCalled();
    });

    it("refuses a bad address while typing, and takes the message away once it is fixed", async () => {
      mount();
      await screen.findByRole("region");
      await type("Git repository (HTTPS)", "user:pw@github.com/x/y");
      expect(screen.getByRole("alert")).toHaveTextContent(
        "Remove the user name and password from the URL",
      );
      await type("Git repository (HTTPS)", "sharkdp/fd");
      expect(screen.queryByRole("alert")).toBeNull();
      expect(
        screen.getByText("Will clone https://github.com/sharkdp/fd"),
      ).toBeInTheDocument();
    });

    it("waits for the field to be left before calling a half-typed owner/repo wrong", async () => {
      mount();
      await screen.findByRole("region");
      await type("Git repository (HTTPS)", "sharkdp");
      expect(screen.queryByRole("alert")).toBeNull();
      await fireEvent.blur(repo());
      expect(screen.getByRole("alert")).toHaveTextContent(
        "Give owner/repo for a GitHub repository",
      );
    });

    it("does not nag about an empty field until Create", async () => {
      mount();
      await screen.findByRole("region");
      await fireEvent.blur(repo());
      expect(screen.queryByRole("alert")).toBeNull();
      await fireEvent.click(submit());
      expect(screen.getAllByRole("alert")[0]).toHaveTextContent(
        "Enter the repository's address, such as owner/repo or https://github.com/owner/repo.",
      );
    });

    it.each([
      [
        "fd",
        "Give owner/repo for a GitHub repository, or the repository's full address.",
      ],
      ["http://github.com/x/y", "Use an https:// URL."],
      ["../fd", "The address can't have . or .. as a name in its path."],
      [
        "github.com./acme/web",
        "Leave out the dot at the end of the host name.",
      ],
    ])(
      "puts the refusal of %j at the field when it is left",
      async (typed, message) => {
        mount();
        await screen.findByRole("region");
        await type("Git repository (HTTPS)", typed);
        await fireEvent.blur(repo());
        expect(screen.getByRole("alert")).toHaveTextContent(message);
      },
    );
  });

  it("checks the memory field too", async () => {
    const { create } = mount();
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
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
      await screen.findByRole("region");
      await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
      await fireEvent.click(submit());
      await vi.waitFor(() =>
        expect(screen.getByRole("alert")).toHaveTextContent(message),
      );
      expect(field(label)).toHaveAttribute("aria-invalid", "true");
      expect(screen.getByRole("region")).toBeInTheDocument();
    },
  );

  it("shows any other refusal for the whole form, and keeps what was typed", async () => {
    mount({ ok: false, message: "Puddle's service isn't answering." });
    await screen.findByRole("region");
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
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(submit());
    expect(screen.getByRole("button", { name: "Creating…" })).toBeDisabled();
    await fireEvent.submit(screen.getByRole("region").querySelector("form")!);
    expect(create).toHaveBeenCalledTimes(1);
    finish({ ok: true, value: workspace("web") });
    await vi.waitFor(() => expect(savedDraft()).toBeNull());
  });

  it("forgets what was typed when it is cancelled, and says so", async () => {
    const onCancel = vi.fn();
    render(CreateWorkspaceForm, {
      props: { store: { create: vi.fn() }, onCancel, settings: settings() },
    } as never);
    await type("Name", "half-done");
    expect(savedDraft()?.name).toBe("half-done");
    await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(savedDraft()).toBeNull();
  });

  describe("an unsent draft", () => {
    const again = () =>
      render(CreateWorkspaceForm, {
        props: { store: { create: vi.fn() }, settings: settings() },
      } as never);

    it("comes back when the screen is left and opened again", async () => {
      const first = render(CreateWorkspaceForm, {
        props: { store: { create: vi.fn() }, settings: settings() },
      } as never);
      await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
      await type("Name", "mine");
      await type("Memory (GiB)", "3");
      first.unmount();
      again();
      expect(field("Git repository (HTTPS)").value).toBe(
        "https://github.com/acme/web.git",
      );
      expect(field("Name").value).toBe("mine");
      expect(field("Memory (GiB)").value).toBe("3");
    });

    it("is replaced by a repository handed in", async () => {
      const first = render(CreateWorkspaceForm, {
        props: { store: { create: vi.fn() }, settings: settings() },
      } as never);
      await type("Name", "mine");
      first.unmount();
      render(CreateWorkspaceForm, {
        props: {
          store: { create: vi.fn() },
          settings: settings(),
          prefill: { url: "https://github.com/acme/api.git", identity: null },
        },
      } as never);
      expect(field("Git repository (HTTPS)").value).toBe(
        "https://github.com/acme/api.git",
      );
      // The focus the screen takes on opening stays on the row the repository made.
      await vi.waitFor(() =>
        expect(document.activeElement).toBe(field("Git repository (HTTPS)")),
      );
    });
  });
  it("replaces the row when another repository is handed in while it is open", async () => {
    const view = render(CreateWorkspaceForm, {
      props: {
        store: { create: vi.fn() },
        settings: settings(),
        prefill: { url: "https://github.com/acme/api.git", identity: null },
      },
    } as never);
    await view.rerender({
      prefill: { url: "https://github.com/acme/web.git", identity: null },
    } as never);
    await vi.waitFor(() =>
      expect(field("Git repository (HTTPS)").value).toBe(
        "https://github.com/acme/web.git",
      ),
    );
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
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(toasts.items).toHaveLength(1));
    const toast = toasts.items[0]!;
    expect(toast.message).toBe(warning);
    expect(toast.tone).toBe("info");
    expect(toast.action?.label).toBe("Open Git tab");
    await toast.action?.run();
    expect(goto).toHaveBeenCalledWith("/workspaces/w-made/git");
  });

  it("says nothing extra when an identity covers the repository or the answer has no identity", async () => {
    const covered = mount({
      ok: true,
      value: workspace("made", {
        identities: [{ identity: "Work", basis: "covers", warning: null }],
      }),
    });
    await screen.findByRole("region");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(submit());
    await vi.waitFor(() => expect(covered.onCreated).toHaveBeenCalled());
    expect(toasts.items).toHaveLength(0);
  });
});
