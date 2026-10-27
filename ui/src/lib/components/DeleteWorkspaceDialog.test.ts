// SPDX-License-Identifier: GPL-3.0-or-later
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it } from "vitest";
import { cleanCheck } from "#lib/testing/fake-workspaces.ts";
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
