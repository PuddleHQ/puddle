// SPDX-License-Identifier: GPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  credential,
  ghSource,
  identity,
} from "#lib/testing/fake-identities.ts";
import { FakeRepos, repoSource, repoView } from "#lib/testing/fake-repos.ts";
import type { AddRepository } from "#lib/stores/checkouts.svelte.ts";
import AddRepositoryDialog from "./AddRepositoryDialog.svelte";

const h = vi.hoisted(() => ({ repos: undefined as unknown }));

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

beforeEach(() => {
  const repos = new FakeRepos();
  repos.repos = [repoView("acme", "web", { identities: [1] })];
  repos.sources = [repoSource({ repo_count: 1 })];
  h.repos = repos;
});
afterEach(cleanup);

function open(
  over: {
    entries?: { url: string; directory: string }[];
    onDisk?: string[];
  } = {},
  result: string | null = null,
) {
  const onSubmit = vi.fn(async (_body: AddRepository) => result);
  render(AddRepositoryDialog, {
    props: {
      open: true,
      entries: over.entries ?? [],
      onDisk: over.onDisk ?? [],
      identities: {
        identities: [work, personal],
        ensureLoaded: async () => {},
      },
      onSubmit,
    },
  } as never);
  return onSubmit;
}

const field = (label: string) =>
  screen.getByLabelText(label) as HTMLInputElement;
const type = (label: string, value: string) =>
  fireEvent.input(field(label), { target: { value } });
const clone = () => screen.getByRole("button", { name: "Clone" });

describe("the Add repository dialog", () => {
  it("takes the address, shows the directory it follows, and sends a clone", async () => {
    const onSubmit = open();
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    expect(field("Directory").placeholder).toBe("web");
    await fireEvent.click(clone());
    await vi.waitFor(() => expect(onSubmit).toHaveBeenCalled());
    expect(onSubmit.mock.calls[0]?.[0]).toMatchObject({
      kind: "clone",
      url: "https://github.com/acme/web.git",
      pull: true,
      push: true,
    });
  });

  it("sends a typed directory, a chosen identity and cleared boxes", async () => {
    const onSubmit = open();
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await type("Directory", "site");
    await fireEvent.change(field("Git identity"), { target: { value: "2" } });
    await fireEvent.click(screen.getByRole("checkbox", { name: "Pull" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "Push" }));
    await fireEvent.click(clone());
    await vi.waitFor(() => expect(onSubmit).toHaveBeenCalled());
    expect(onSubmit.mock.calls[0]?.[0]).toMatchObject({
      directory: "site",
      identity_id: 2,
      pull: false,
      push: false,
    });
  });

  it("takes the repository and the identity that listed it from the picker", async () => {
    open();
    await screen.findByRole("dialog");
    await fireEvent.click(screen.getByText("Choose from your repositories"));
    await fireEvent.click(
      await screen.findByRole("button", { name: /acme\/web/ }),
    );
    expect(field("Git repository (HTTPS)").value).toBe(
      "https://github.com/acme/web",
    );
    expect(field("Directory")).toHaveFocus();
    expect(
      screen.getByRole("option", { name: /Work \(listed this repository\)/ }),
    ).toBeInTheDocument();
  });

  it("warns about an Azure DevOps project with a space in it", async () => {
    open();
    await screen.findByRole("dialog");
    await type(
      "Git repository (HTTPS)",
      "https://dev.azure.com/contoso/Shop%20Floor/_git/scanner",
    );
    expect(screen.getByTestId("table-cannot-hold")).toBeInTheDocument();
  });

  it("refuses the same repository and a taken directory, and sends nothing", async () => {
    const onSubmit = open({
      entries: [{ url: "https://github.com/acme/api.git", directory: "api" }],
      onDisk: ["web"],
    });
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/api");
    await fireEvent.click(clone());
    expect(
      await screen.findByText(/already on the list, as api/),
    ).toBeInTheDocument();
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    expect(
      await screen.findByText(/web already exists in the workspace/),
    ).toBeInTheDocument();
    await fireEvent.click(clone());
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("shows the service's refusal and keeps the dialog open", async () => {
    open({}, "The directory is taken.");
    await screen.findByRole("dialog");
    await type("Git repository (HTTPS)", "https://github.com/acme/web.git");
    await fireEvent.click(clone());
    expect(
      await screen.findByText("The directory is taken."),
    ).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    await type("Directory", "other");
    expect(screen.queryByText("The directory is taken.")).toBeNull();
  });
});
