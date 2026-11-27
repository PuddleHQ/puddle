// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
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

  it("lists each changed repository of four by its directory with its own unpushed commits", async () => {
    const none = { items: [], more: 0 };
    const repo = (dir: string, over: Record<string, unknown> = {}) => ({
      dir,
      clean: true,
      uncommitted: none,
      unpushed: none,
      stashes: none,
      ...over,
    });
    open({
      check: cleanCheck("mono", {
        clean: false,
        fingerprint: "fp-four",
        repos: [
          repo("mono-api"),
          repo("mono-web", {
            clean: false,
            unpushed: { items: ["a1 One", "b2 Two"], more: 3 },
          }),
          repo("mono-shared", {
            clean: false,
            uncommitted: { items: ["?? notes.md"], more: 0 },
          }),
          repo("mono-assets", {
            clean: false,
            unpushed: { items: ["c3 Logo"], more: 0 },
          }),
        ],
      }),
    });
    const dialog = await screen.findByRole("alertdialog");
    const unpushed = within(dialog)
      .getAllByText("Commits that are on no remote branch")
      .map((b) => b.closest("li")!);
    expect(unpushed).toHaveLength(2);
    expect(unpushed[0]).toHaveTextContent("in mono-web");
    expect(unpushed[0]).toHaveTextContent("a1 One");
    expect(unpushed[0]).toHaveTextContent("b2 Two");
    expect(unpushed[0]).toHaveTextContent("and 3 more");
    expect(unpushed[1]).toHaveTextContent("in mono-assets");
    expect(unpushed[1]).toHaveTextContent("c3 Logo");
    expect(unpushed[1]).not.toHaveTextContent("more");
    const uncommitted = within(dialog)
      .getByText("Uncommitted changes")
      .closest("li")!;
    expect(uncommitted).toHaveTextContent("in mono-shared");
    expect(uncommitted).toHaveTextContent("?? notes.md");
    expect(dialog).not.toHaveTextContent("mono-api");
    expect(dialog).toHaveTextContent(
      "The unsaved work check ignores git-ignored files.",
    );
  });

  it("says the check ignores git-ignored files, for a dirty and a clean report", async () => {
    open({ check: dirtyCheck("web") });
    expect(await screen.findByRole("note")).toHaveTextContent(
      "The unsaved work check ignores git-ignored files.",
    );
    cleanup();
    open({ check: cleanCheck("web") });
    expect(await screen.findByRole("note")).toHaveTextContent(
      "The unsaved work check ignores git-ignored files.",
    );
  });

  it("leaves the git-ignored note out when the disk is gone and nothing was checked", async () => {
    open({ check: cleanCheck("web", { volume_missing: true }) });
    await screen.findByRole("alertdialog");
    expect(screen.queryByRole("note")).toBeNull();
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
