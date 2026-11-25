// SPDX-License-Identifier: AGPL-3.0-or-later
// The generated workspace id is for the machine: a person sees the name. This sweep renders the
// screens that show a workspace (the card in every state, its dialogs) with an id that no name
// contains, and fails when the id shows up in text, an accessible name, a tooltip or a field. A
// `<details>` disclosure (a "Volume name" for support) may hold it: it is closed until opened.
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = await vi.hoisted(async () => {
  const ws = await import("#lib/testing/fake-workspaces.ts");
  return { api: new ws.FakeWorkspaces(), workspace: ws.workspace };
});

vi.mock("$app/navigation", () => ({ goto: vi.fn() }));
vi.mock("#lib/stores/workspaces.svelte.ts", async (original) => {
  const mod =
    await original<typeof import("#lib/stores/workspaces.svelte.ts")>();
  return {
    ...mod,
    workspaces: new mod.WorkspaceStore({ api: h.api as never }),
  };
});

import { workspaceActions as actions } from "#lib/stores/workspace-actions.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import type { Workspace } from "#lib/workspaces/model.ts";
import WorkspaceDialogs from "./WorkspaceDialogs.svelte";
import WorkspaceRow from "./WorkspaceRow.svelte";

const { api, workspace } = h;
const ID = "eqbtm0xh";
const NAME = "qa-first";
const STATUSES: Workspace["status"][] = [
  "created",
  "starting",
  "running",
  "draining",
  "paused",
  "stopped",
  "crashed",
  "volume_missing",
];
const BUSY: NonNullable<Workspace["busy"]>[] = ["creating", "deleting"];

/** Everything a person reads or hears in `root`, minus what sits closed inside a `<details>`. */
function seen(root: Element): string[] {
  const parts: string[] = [];
  const walk = (node: Node) => {
    if (node.nodeType === Node.TEXT_NODE) {
      parts.push(node.textContent ?? "");
      return;
    }
    if (!(node instanceof Element)) return;
    if (node.tagName === "DETAILS") {
      const summary = node.querySelector("summary");
      if (summary) parts.push(summary.textContent ?? "");
      return;
    }
    for (const attr of ["aria-label", "title", "placeholder", "alt", "value"]) {
      const text = node.getAttribute(attr);
      if (text) parts.push(text);
    }
    if (node instanceof HTMLInputElement) parts.push(node.value);
    node.childNodes.forEach(walk);
  };
  walk(root);
  return parts;
}

function expectNoId(root: Element, where: string) {
  for (const part of seen(root)) {
    expect(part, where).not.toContain(ID);
  }
}

beforeEach(async () => {
  api.list = [workspace(NAME, { id: ID, status: "running" })];
  api.calls = [];
  api.check = null;
  await workspaces.refresh();
  actions.renameOpen = false;
  actions.renaming = null;
  actions.deleteOpen = false;
  actions.deleting = null;
});
afterEach(cleanup);

const NOOPS = {
  onStart: () => {},
  onStop: () => {},
  onConnect: () => {},
  onDismiss: () => {},
};

describe("the workspace id stays out of what a person reads", () => {
  it("the sweep can tell a leak from a disclosure", () => {
    const { container } = render(WorkspaceRow, {
      props: {
        workspace: workspace(NAME, { id: ID, status: "volume_missing" }),
        now: 0,
        ...NOOPS,
      },
    });
    const leaking = document.createElement("p");
    leaking.textContent = `agent@${ID}`;
    container.append(leaking);
    expect(seen(container).join(" ")).toContain(ID);
    leaking.remove();
    expect(container.querySelector("details")).toHaveTextContent(ID);
    expect(seen(container).join(" ")).not.toContain(ID);
  });

  for (const status of STATUSES) {
    it(`the card of a ${status} workspace`, () => {
      const { container } = render(WorkspaceRow, {
        props: {
          workspace: workspace(NAME, { id: ID, status }),
          now: 0,
          ...NOOPS,
        },
      });
      expect(container).toHaveTextContent(NAME);
      expectNoId(container, status);
    });
  }

  for (const busy of BUSY) {
    it(`the card of a workspace that is ${busy}`, () => {
      const { container } = render(WorkspaceRow, {
        props: {
          workspace: workspace(NAME, { id: ID, busy }),
          now: 0,
          ...NOOPS,
        },
      });
      expectNoId(container, busy);
    });
  }

  it("the rename dialog", async () => {
    render(WorkspaceDialogs);
    actions.openRename(workspaces.byId(ID)!);
    const dialog = await screen.findByRole("dialog", {
      name: "Rename workspace",
    });
    expectNoId(dialog, "rename");
    const field = within(dialog).getByRole("textbox", { name: "Name" });
    await fireEvent.input(field, { target: { value: "x".repeat(61) } });
    expectNoId(dialog, "rename, refused");
  });

  it("the delete dialog", async () => {
    render(WorkspaceDialogs);
    await actions.askDelete(workspaces.byId(ID)!);
    const dialog = await screen.findByRole("alertdialog");
    expectNoId(dialog, "delete");
  });
});
