// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import AddRuleDialog from "./AddRuleDialog.svelte";

afterEach(cleanup);

describe("the Add rule dialog without rule sets", () => {
  it("offers only one workspace and every workspace as who", async () => {
    const onSubmit = vi.fn(async () => null);
    render(AddRuleDialog, {
      props: { open: true, workspaces: [], now: () => 0, onSubmit },
    } as never);
    await screen.findByRole("dialog", { name: "Add a rule" });
    const who = screen.getByRole("group", { name: "Who" });
    expect(
      within(who)
        .getAllByRole("radio")
        .map((radio) => (radio as HTMLInputElement).value),
    ).toEqual(["workspace", "global"]);
    await fireEvent.click(screen.getByLabelText(/Every workspace/));
    expect(screen.getByLabelText(/Every workspace/)).toBeChecked();
  });

  it("sends a precedence only when one was chosen, and clears it with the form", async () => {
    const onSubmit = vi.fn(async (_rule: unknown) => null);
    render(AddRuleDialog, {
      props: { open: true, workspaces: [], now: () => 0, onSubmit },
    } as never);
    await screen.findByRole("dialog", { name: "Add a rule" });
    await fireEvent.input(screen.getByLabelText("Host"), {
      target: { value: "example.com" },
    });
    await fireEvent.input(screen.getByLabelText("Workspace name"), {
      target: { value: "box" },
    });
    const precedence = screen.getByLabelText("Precedence") as HTMLSelectElement;
    expect([...precedence.options].map((option) => option.value)).toEqual([
      "",
      "1",
      "2",
      "3",
      "4",
      "5",
      "6",
      "7",
      "8",
      "9",
    ]);
    await fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    expect(onSubmit.mock.calls[0]?.[0]).not.toHaveProperty("precedence");
    await fireEvent.change(precedence, { target: { value: "7" } });
    await fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    expect(onSubmit.mock.calls[1]?.[0]).toMatchObject({ precedence: 7 });
  });
});
