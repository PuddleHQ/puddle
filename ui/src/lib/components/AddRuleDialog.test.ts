// SPDX-License-Identifier: GPL-3.0-or-later
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
});
