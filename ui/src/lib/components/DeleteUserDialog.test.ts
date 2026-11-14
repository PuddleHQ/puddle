// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import DeleteUserDialog from "./DeleteUserDialog.svelte";

afterEach(cleanup);

const open = (props: Record<string, unknown> = {}) => {
  const onConfirm = vi.fn();
  const onCancel = vi.fn();
  render(DeleteUserDialog, {
    open: true,
    name: "Sam",
    onConfirm,
    onCancel,
    ...props,
  } as never);
  return { onConfirm, onCancel };
};

describe("the delete-user dialog", () => {
  it("says what is lost and that it cannot be undone, with Cancel focused first", async () => {
    open();
    const dialog = await screen.findByRole("alertdialog", {
      name: "Delete Sam?",
    });
    expect(dialog).toHaveTextContent(
      "workspaces and everything on their disks",
    );
    expect(dialog).toHaveTextContent("can't be undone");
    await vi.waitFor(() =>
      expect(document.activeElement).toBe(
        screen.getByRole("button", { name: "Cancel" }),
      ),
    );
  });

  it("deletes only once the exact name is typed", async () => {
    const { onConfirm } = open();
    const button = await screen.findByRole("button", { name: "Delete user" });
    const input = screen.getByLabelText(/Type/);
    expect(button).toBeDisabled();
    await fireEvent.input(input, { target: { value: "sam" } });
    expect(button).toBeDisabled();
    await fireEvent.input(input, { target: { value: "Sam" } });
    expect(button).toBeEnabled();
    await fireEvent.click(button);
    expect(onConfirm).toHaveBeenCalledExactlyOnceWith("Sam");
  });

  it("waits while working and shows why a delete failed", async () => {
    open({ working: true, error: "The name does not match." });
    const button = await screen.findByRole("button", { name: "Deleting…" });
    await fireEvent.input(screen.getByLabelText(/Type/), {
      target: { value: "Sam" },
    });
    expect(button).toBeDisabled();
    expect(screen.getByRole("alert")).toHaveTextContent("does not match");
  });

  it("tells its owner when it is cancelled", async () => {
    const { onCancel } = open();
    await fireEvent.click(
      await screen.findByRole("button", { name: "Cancel" }),
    );
    expect(onCancel).toHaveBeenCalled();
  });
});
