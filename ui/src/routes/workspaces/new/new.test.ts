// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { workspace } from "#lib/testing/fake-workspaces.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { clearDraft } from "#lib/workspaces/draft.ts";
import Page from "./+page.svelte";

const h = vi.hoisted(() => ({
  goto: vi.fn(),
  search: "",
}));
vi.mock("$app/navigation", () => ({ goto: h.goto }));
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(`http://localhost/workspaces/new${h.search}`);
    },
  },
}));

afterEach(() => {
  cleanup();
  clearDraft();
  h.goto.mockReset();
  h.search = "";
  vi.restoreAllMocks();
});

describe("the New workspace screen", () => {
  it("shows the resources inline, with no submenu", async () => {
    render(Page);
    expect(await screen.findByLabelText("Memory (GiB)")).toBeVisible();
    expect(screen.getByLabelText("Image")).toBeVisible();
    expect(
      screen.queryByText(/Start folder, image, memory and CPUs/),
    ).toBeNull();
  });

  it("takes the repository from the link", async () => {
    h.search = "?url=https%3A%2F%2Fgithub.com%2Facme%2Fweb";
    render(Page);
    expect(await screen.findByLabelText("Git repository (HTTPS)")).toHaveValue(
      "https://github.com/acme/web",
    );
  });

  it("goes back to the list on Cancel", async () => {
    render(Page);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Cancel" }),
    );
    expect(h.goto).toHaveBeenCalledWith("/workspaces");
  });

  it("goes to the list once a workspace was accepted", async () => {
    vi.spyOn(workspaces, "create").mockResolvedValue({
      ok: true,
      value: workspace("web"),
    });
    render(Page);
    await fireEvent.input(
      await screen.findByLabelText("Git repository (HTTPS)"),
      {
        target: { value: "https://github.com/acme/web" },
      },
    );
    await fireEvent.click(
      await screen.findByRole("button", { name: "Create workspace" }),
    );
    await vi.waitFor(() => expect(h.goto).toHaveBeenCalledWith("/workspaces"));
  });

  it("opens the new workspace when the repository came from a link", async () => {
    vi.spyOn(workspaces, "create").mockResolvedValue({
      ok: true,
      value: workspace("web"),
    });
    h.search = "?url=https%3A%2F%2Fgithub.com%2Facme%2Fweb";
    render(Page);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Create workspace" }),
    );
    await vi.waitFor(() =>
      expect(h.goto).toHaveBeenCalledWith("/workspaces/w-web"),
    );
  });
});
