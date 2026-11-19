// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { rule } from "#lib/testing/fake-rules.ts";
import AddRuleDialog from "./AddRuleDialog.svelte";
import { chooseWorkspace } from "#lib/testing/choose-workspace.ts";

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

  it("hides the precedence field in the shipped configuration", async () => {
    const onSubmit = vi.fn(async (_rule: unknown) => null);
    render(AddRuleDialog, {
      props: { open: true, workspaces: [], now: () => 0, onSubmit },
    } as never);
    await screen.findByRole("dialog", { name: "Add a rule" });
    expect(screen.queryByLabelText("Precedence")).toBeNull();
    await fireEvent.input(screen.getByLabelText("Host"), {
      target: { value: "example.com" },
    });
    await chooseWorkspace(screen.getByLabelText("Workspace"), "box");
    await fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    expect(onSubmit.mock.calls[0]?.[0]).not.toHaveProperty("precedence");
  });

  it("sends a precedence only when one was chosen, and clears it with the form", async () => {
    const onSubmit = vi.fn(async (_rule: unknown) => null);
    render(AddRuleDialog, {
      props: {
        open: true,
        workspaces: [],
        now: () => 0,
        onSubmit,
        showPrecedence: true,
      },
    } as never);
    await screen.findByRole("dialog", { name: "Add a rule" });
    await fireEvent.input(screen.getByLabelText("Host"), {
      target: { value: "example.com" },
    });
    await chooseWorkspace(screen.getByLabelText("Workspace"), "box");
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

describe("the dialog edits a rule it is given", () => {
  const sets = [
    {
      id: 4,
      name: "Docs",
      fill: { kind: "solid", color: "#c04f7a" },
      everywhere: false,
    },
  ];

  it("fills the form from the rule, and says Edit and Save", async () => {
    const onSubmit = vi.fn(async (_rule: unknown) => null);
    const { component } = render(AddRuleDialog, {
      props: { open: false, workspaces: [], sets, now: () => 0, onSubmit },
    } as never);
    (component as unknown as { edit: (r: unknown) => void }).edit(
      rule(7, {
        pattern: ".example.org",
        pattern_kind: "suffix",
        effect: "deny",
        scope: { type: "set", set: 4 },
      }),
    );
    await screen.findByRole("dialog", { name: "Edit rule" });
    expect(screen.getByLabelText("Host")).toHaveValue("*.example.org");
    expect(screen.getByLabelText("Deny")).toBeChecked();
    expect(screen.getByLabelText(/In rule set/)).toBeChecked();
    await fireEvent.click(screen.getByRole("button", { name: "Save rule" }));
    expect(onSubmit).toHaveBeenCalledWith({
      pattern: "*.example.org",
      effect: "deny",
      scope: { type: "set", set: 4 },
      expires_at: null,
    });
  });

  it("starts the next add empty", async () => {
    const onSubmit = vi.fn(async (_rule: unknown) => null);
    const { component } = render(AddRuleDialog, {
      props: { open: false, workspaces: [], now: () => 0, onSubmit },
    } as never);
    const api = component as unknown as {
      edit: (r: unknown) => void;
      clear: () => void;
    };
    api.edit(rule(7));
    await screen.findByRole("dialog", { name: "Edit rule" });
    api.clear();
    await waitFor(() => expect(screen.getByLabelText("Host")).toHaveValue(""));
    expect(screen.getByRole("button", { name: "Add rule" })).toBeVisible();
  });

  it("preselects the rule's precedence when the field is shown", async () => {
    const onSubmit = vi.fn(async (_rule: unknown) => null);
    const { component } = render(AddRuleDialog, {
      props: {
        open: false,
        workspaces: [],
        now: () => 0,
        onSubmit,
        showPrecedence: true,
      },
    } as never);
    (component as unknown as { edit: (r: unknown) => void }).edit(
      rule(7, { precedence: 8 }),
    );
    await screen.findByRole("dialog", { name: "Edit rule" });
    expect(screen.getByLabelText("Precedence")).toHaveValue("8");
    await fireEvent.click(screen.getByRole("button", { name: "Save rule" }));
    expect(onSubmit.mock.calls[0]?.[0]).toMatchObject({ precedence: 8 });
  });
});
