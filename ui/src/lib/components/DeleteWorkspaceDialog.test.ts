// SPDX-License-Identifier: GPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanCheck, dirtyCheck } from "#lib/testing/fake-workspaces.ts";
import DeleteWorkspaceDialog from "./DeleteWorkspaceDialog.svelte";

afterEach(cleanup);

const show = (removes: Record<string, number> | null) =>
  render(DeleteWorkspaceDialog, {
    open: true,
    name: "web",
    check: cleanCheck("web"),
    removes: removes as never,
    onConfirm: () => {},
    onCancel: () => {},
  });

describe("what Puddle removes beside the disk", () => {
  it("lists the variables, secrets and rules it keeps for the workspace", async () => {
    show({ variables: 2, secrets: 1, rules: 3 });
    const dialog = await screen.findByRole("alertdialog", {
      name: "Delete web?",
    });
    expect(dialog).toHaveTextContent(
      "Puddle also removes what it keeps for web: 2 variables, 1 secret and 3 rules.",
    );
    expect(dialog).toHaveTextContent("leaves this computer's credential store");
  });

  it("says nothing when there is none, or when it could not be counted", async () => {
    show({ variables: 0, secrets: 0, rules: 0 });
    expect((await screen.findByRole("alertdialog")).textContent).not.toContain(
      "Puddle also removes",
    );
    cleanup();
    show(null);
    expect((await screen.findByRole("alertdialog")).textContent).not.toContain(
      "Puddle also removes",
    );
  });
});

describe("what the dialog says about the disk", () => {
  const open = (props: Partial<Parameters<typeof render>[1]> = {}) => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(DeleteWorkspaceDialog, {
      open: true,
      name: "web",
      check: cleanCheck("web"),
      onConfirm,
      onCancel,
      ...props,
    } as never);
    return { onConfirm, onCancel };
  };

  it("says there is nothing to lose when the disk is already gone", async () => {
    open({ check: cleanCheck("web", { volume_missing: true }) });
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Its disk is already gone");
    expect(dialog).toHaveTextContent("nothing is lost");
    expect(
      screen.getByRole("checkbox", { name: "Delete web" }),
    ).toBeInTheDocument();
  });

  it("lists what would be lost and asks for an explicit yes", async () => {
    const { onConfirm } = open({ check: dirtyCheck("web") });
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("You would lose");
    expect(dialog).toHaveTextContent("Uncommitted changes");
    expect(dialog).toHaveTextContent("and 3 more");
    const button = screen.getByRole("button", { name: "Delete workspace" });
    expect(button).toBeDisabled();
    await fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I understand this work will be lost: delete web",
      }),
    );
    expect(button).toBeEnabled();
    await fireEvent.click(button);
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("owns up when it could not check everything, and that nothing was found", async () => {
    open({
      check: cleanCheck("web", {
        clean: false,
        errors: ["could not read /work/a"],
      }),
      error: "the server said no",
    });
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Nothing was found, but not everything");
    expect(dialog).toHaveTextContent("could not read /work/a");
    expect(screen.getByRole("alert")).toHaveTextContent("the server said no");
  });

  it("shows progress and keeps the button off while deleting", async () => {
    open({ check: cleanCheck("web"), working: true });
    await screen.findByRole("alertdialog");
    await fireEvent.click(
      screen.getByRole("checkbox", { name: "Delete web and its disk" }),
    );
    expect(screen.getByRole("button", { name: "Deleting…" })).toBeDisabled();
  });

  it("asks again after the check changes, and cancels on Cancel", async () => {
    const { onCancel } = open({ check: cleanCheck("web") });
    await screen.findByRole("alertdialog");
    await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalled();
  });
});
