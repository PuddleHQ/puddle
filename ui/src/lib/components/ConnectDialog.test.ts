// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { session } from "#lib/session/session.svelte.ts";
import { workspace } from "#lib/testing/fake-workspaces.ts";
import type { Workspace } from "#lib/workspaces/model.ts";
import ConnectDialog from "./ConnectDialog.svelte";

afterEach(() => {
  cleanup();
  session.mode = "checking";
});

function open(
  over: Partial<Workspace> = {},
  server: "microsoft" | "code_server" | null = "code_server",
  editorProblem: { title: string; detail: string } | null = null,
) {
  const onBrowser = vi.fn();
  const onDesktop = vi.fn();
  const onDirectSsh = vi.fn();
  render(ConnectDialog, {
    props: {
      open: true,
      workspace: workspace("shop", over),
      server,
      editorProblem,
      onBrowser,
      onDesktop,
      onDirectSsh,
    },
  } as never);
  return { onBrowser, onDesktop, onDirectSsh };
}

const gone = () =>
  vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

describe("the Connect dialog", () => {
  it("offers the VS Code button on the host computer", async () => {
    session.mode = "local";
    open({ direct_ssh: true });
    expect(
      await screen.findByRole("button", { name: /Open in VS Code/ }),
    ).toBeTruthy();
  });

  it("shows no VS Code button to a browser signed in from another computer", async () => {
    session.mode = "signed-in";
    open({ direct_ssh: true });
    await screen.findByRole("dialog");
    expect(
      screen.queryByRole("button", { name: /Open in VS Code/ }),
    ).toBeNull();
    expect(
      screen.getByText(/opens only on the computer Puddle runs on/),
    ).toBeTruthy();
    expect(
      screen.getByRole("button", { name: /Open isolated VS Code/ }),
    ).toBeTruthy();
  });

  it("names the server the browser editor uses, when known", async () => {
    open({}, "microsoft");
    await screen.findByRole("dialog");
    expect(
      screen.getByText(/downloaded by Puddle from Microsoft/),
    ).toBeVisible();
  });

  it("says nothing about a server before it is known", async () => {
    open({}, null);
    await screen.findByRole("dialog");
    expect(
      screen.queryByText(/downloaded by Puddle from Microsoft/),
    ).toBeNull();
    expect(screen.getByText(/It is not your own VS Code/)).toBeVisible();
  });

  it("says why the browser editor cannot be installed, and still lets the user try", async () => {
    const { onBrowser } = open({}, "microsoft", {
      title:
        "The browser editor could not be installed in shop: the download does not match",
      detail: "Puddle tries again.",
    });
    const note = await screen.findByRole("alert");
    expect(note).toHaveTextContent(
      "The browser editor could not be installed in shop: the download does not match.",
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Open isolated VS Code" }),
    );
    expect(onBrowser).toHaveBeenCalledTimes(1);
  });

  it("shows no install note while the editor has no problem", async () => {
    open();
    await screen.findByRole("dialog");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("closes when the user follows the link to the settings", async () => {
    open();
    await screen.findByRole("dialog");
    const link = screen.getByRole("link", {
      name: "Change the server in Settings",
    });
    expect(link).toHaveAttribute("href", "/settings/editor-server#set-server");
    link.addEventListener("click", (event) => event.preventDefault());
    await fireEvent.click(link);
    await gone();
  });

  it("closes with the Close button", async () => {
    open();
    await screen.findByRole("dialog");
    await fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await gone();
  });

  it("asks for direct SSH to be turned on without changing the switch itself", async () => {
    const { onDirectSsh } = open({ direct_ssh: false });
    await screen.findByRole("dialog");
    const box = screen.getByRole("checkbox") as HTMLInputElement;
    await fireEvent.click(box);
    expect(onDirectSsh).toHaveBeenCalledWith(
      expect.objectContaining({ name: "shop" }),
      true,
    );
    expect(box.checked).toBe(false);
  });

  it("opens the browser editor and closes, with direct SSH off", async () => {
    const { onBrowser } = open({ direct_ssh: false });
    await screen.findByRole("dialog");
    const button = screen.getByRole("button", {
      name: "Open isolated VS Code",
    });
    expect(button).toBeEnabled();
    await fireEvent.click(button);
    expect(onBrowser).toHaveBeenCalledWith(
      expect.objectContaining({ name: "shop" }),
    );
    await gone();
  });

  it("opens desktop VS Code and closes once direct SSH is on", async () => {
    const { onDesktop } = open({ direct_ssh: true });
    await screen.findByRole("dialog");
    await fireEvent.click(
      screen.getByRole("button", { name: "Open in VS Code" }),
    );
    expect(onDesktop).toHaveBeenCalledWith(
      expect.objectContaining({ name: "shop" }),
    );
    await gone();
  });

  it("keeps desktop VS Code disabled while direct SSH is off", async () => {
    open({ direct_ssh: false });
    await screen.findByRole("dialog");
    expect(
      screen.getByRole("button", { name: "Open in VS Code" }),
    ).toBeDisabled();
  });
});
