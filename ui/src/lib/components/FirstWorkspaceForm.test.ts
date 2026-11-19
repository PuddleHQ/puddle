// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { workspace } from "#lib/testing/fake-workspaces.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import FirstWorkspaceForm from "./FirstWorkspaceForm.svelte";

afterEach(cleanup);

function mount(create = vi.fn(), list: unknown[] = []) {
  const onMore = vi.fn();
  render(FirstWorkspaceForm, {
    props: { store: { create, list: list as never }, onMore },
  });
  return { create, onMore };
}

const type = (value: string) =>
  fireEvent.input(screen.getByRole("textbox", { name: "Repository address" }), {
    target: { value },
  });

describe("FirstWorkspaceForm", () => {
  it("creates a workspace named after the repository from the address alone", async () => {
    const { create } = mount(
      vi.fn().mockResolvedValue({ ok: true, value: workspace("api") }),
    );
    await type("https://github.com/acme/api.git");
    await fireEvent.click(
      screen.getByRole("button", { name: "Create workspace" }),
    );
    expect(create).toHaveBeenCalledWith({
      name: "api",
      repositories: [{ url: "https://github.com/acme/api.git" }],
    });
  });

  it("says which address it will clone when the shorthand differs", async () => {
    mount();
    await type("acme/api");
    expect(
      screen.getByText("Will clone https://github.com/acme/api"),
    ).toBeInTheDocument();
  });

  it("shows the service's refusal at the field and keeps what was typed", async () => {
    mount(vi.fn().mockResolvedValue({ ok: false, message: "name taken" }));
    await type("https://github.com/acme/api.git");
    await fireEvent.click(
      screen.getByRole("button", { name: "Create workspace" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent("name taken");
    expect(
      screen.getByRole("textbox", { name: "Repository address" }),
    ).toHaveValue("https://github.com/acme/api.git");
  });

  it("opens the full form from More options", async () => {
    const { onMore } = mount();
    await fireEvent.click(screen.getByRole("button", { name: "More options" }));
    expect(onMore).toHaveBeenCalledOnce();
  });

  it("waits to call a half-typed owner an error until the field is left", async () => {
    mount();
    await type("sharkdp");
    expect(screen.queryByRole("alert")).toBeNull();
    await fireEvent.blur(
      screen.getByRole("textbox", { name: "Repository address" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(/owner\/repo/);
  });

  it("refuses a name another workspace has, without asking the service", async () => {
    const { create } = mount(vi.fn(), [workspace("api")]);
    await type("https://github.com/acme/api.git");
    await fireEvent.click(
      screen.getByRole("button", { name: "Create workspace" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(/already/);
    expect(create).not.toHaveBeenCalled();
  });

  it("says which hosts it allowed for the clone, and clears the field", async () => {
    mount(
      vi.fn().mockResolvedValue({
        ok: true,
        value: workspace("api", {
          allowed_hosts: [{ host: "github.com", rule_id: 1 }],
        }),
      }),
    );
    await type("https://github.com/acme/api.git");
    await fireEvent.click(
      screen.getByRole("button", { name: "Create workspace" }),
    );
    await vi.waitFor(() =>
      expect(toasts.items.map((t) => t.message).join(" ")).toContain(
        "Puddle allowed github.com",
      ),
    );
    expect(
      screen.getByRole("textbox", { name: "Repository address" }),
    ).toHaveValue("");
  });

  it("sends one request when Create is pressed twice while it works", async () => {
    const { create } = mount(vi.fn().mockReturnValue(new Promise(() => {})));
    await type("https://github.com/acme/api.git");
    const form = screen
      .getByRole("button", { name: "Create workspace" })
      .closest("form")!;
    await fireEvent.submit(form);
    await fireEvent.submit(form);
    expect(create).toHaveBeenCalledOnce();
  });
});
