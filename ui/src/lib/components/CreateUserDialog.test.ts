// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import CreateUserDialog from "./CreateUserDialog.svelte";

afterEach(cleanup);

const open = (props: Record<string, unknown> = {}) => {
  const onCreate = vi.fn();
  render(CreateUserDialog, { open: true, onCreate, ...props } as never);
  return onCreate;
};

describe("the add-user dialog", () => {
  it("asks for a name only, never a password", async () => {
    open();
    await screen.findByRole("dialog", { name: "Add a user" });
    expect(screen.getByLabelText("Name")).toBeInTheDocument();
    expect(document.querySelector('input[type="password"]')).toBeNull();
  });

  it("makes the user with the trimmed name, and only with one", async () => {
    const onCreate = open();
    const button = await screen.findByRole("button", { name: "Add user" });
    expect(button).toBeDisabled();
    await fireEvent.input(screen.getByLabelText("Name"), {
      target: { value: "  Sam  " },
    });
    await fireEvent.click(button);
    expect(onCreate).toHaveBeenCalledExactlyOnceWith("Sam");
  });

  it("does nothing on a blank name or while it is working", async () => {
    const onCreate = open({ working: true });
    await screen.findByRole("dialog");
    const input = screen.getByLabelText("Name");
    await fireEvent.input(input, { target: { value: "Sam" } });
    expect(screen.getByRole("button", { name: "Adding…" })).toBeDisabled();
    await fireEvent.submit(input.closest("form")!);
    expect(onCreate).not.toHaveBeenCalled();
    await fireEvent.input(input, { target: { value: " " } });
    await fireEvent.submit(input.closest("form")!);
    expect(onCreate).not.toHaveBeenCalled();
  });

  it("shows why a create was refused, and closes on Cancel", async () => {
    open({ error: "That name is taken." });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "That name is taken.",
    );
    await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });
});
