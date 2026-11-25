// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  FakeRuleSets,
  builtIn,
  mine,
  systemHost,
} from "#lib/testing/fake-rule-sets.ts";
import { RuleSetsStore } from "#lib/stores/rule-sets.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import WorkspaceSets from "./WorkspaceSets.svelte";

let api: FakeRuleSets;
let store: RuleSetsStore;

beforeEach(async () => {
  api = new FakeRuleSets();
  api.sets = [
    builtIn("github", { name: "GitHub" }),
    mine(4, {
      name: "Work",
      overrides: [{ workspace: "api" as never, enabled: false }],
    }),
  ];
  api.system = [
    systemHost("open-vsx.org"),
    systemHost("marketplace.visualstudio.com", {
      reason: "direct_ssh",
      reason_text: "Direct SSH is on for this workspace.",
      workspace: "other" as never,
    }),
  ];
  store = new RuleSetsStore({ api: api as never });
  await store.refresh();
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});
afterEach(cleanup);

describe("a workspace's rule sets", () => {
  it("says the rule sets are loading until the first read is back", () => {
    const unread = new RuleSetsStore({ api: new FakeRuleSets() as never });
    render(WorkspaceSets, {
      props: { store: unread, workspace: "api" },
    } as never);
    expect(screen.getByText(/Loading rule sets/)).toBeInTheDocument();
  });

  it("shows each set's colour as a dot before its name", async () => {
    render(WorkspaceSets, { props: { store, workspace: "api" } } as never);
    const work = (await screen.findByText("Work")).closest(".set-dotted");
    expect(work).toHaveStyle("--set-fill: #2ea06e");
    expect(screen.getByText("GitHub").closest(".set-dotted")).toHaveStyle(
      "--set-fill: #2f80c9",
    );
  });

  it("says a read failed instead of showing an empty list, and warns about a stale one", async () => {
    store.loadError = "Disk is full.";
    store.status = "failed";
    render(WorkspaceSets, { props: { store, workspace: "api" } } as never);
    expect(
      await screen.findByText(/Couldn't read the rule sets\. Disk is full\./),
    ).toBeInTheDocument();
    store.status = "ready";
    expect(
      await screen.findByText(/Couldn't refresh the rule sets/),
    ).toBeInTheDocument();
    expect(screen.getByText("Work")).toBeInTheDocument();
  });

  it("shows each set's state there and only the System managed hosts that apply", () => {
    render(WorkspaceSets, { props: { store, workspace: "api" } } as never);
    const github = screen.getByRole("group", { name: "GitHub in api" });
    expect(
      within(github).getByRole("button", { name: "Follow (off)" }),
    ).toHaveAttribute("aria-pressed", "true");
    const work = screen.getByRole("group", { name: "Work in api" });
    expect(within(work).getByRole("button", { name: "Off" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(within(work).getByRole("button", { name: "On" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
    const system = screen.getByRole("region", { name: "System managed" });
    expect(within(system).getByText("open-vsx.org")).toBeInTheDocument();
    expect(
      within(system).queryByText("marketplace.visualstudio.com"),
    ).toBeNull();
  });

  it("switches a set there, and back to following every workspace", async () => {
    render(WorkspaceSets, { props: { store, workspace: "api" } } as never);
    await fireEvent.click(
      within(screen.getByRole("group", { name: "GitHub in api" })).getByRole(
        "button",
        { name: "On" },
      ),
    );
    await waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe("GitHub: on in api."),
    );
    api.closes = [1, 2];
    const work = () => screen.getByRole("group", { name: "Work in api" });
    await fireEvent.click(
      within(work()).getByRole("button", { name: /^Follow/ }),
    );
    await waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe(
        "Work: follows every workspace in api; it decided 2 waiting requests.",
      ),
    );
    api.refuse = { status: 500, message: "boom" };
    await fireEvent.click(within(work()).getByRole("button", { name: "Off" }));
    await waitFor(() => expect(toasts.items.at(-1)?.message).toBe("Boom."));
  });

  it("says when nothing is allowed by Puddle itself", () => {
    store.system = [];
    render(WorkspaceSets, { props: { store, workspace: "api" } } as never);
    expect(
      screen.getByText("Puddle allows nothing by itself right now."),
    ).toBeInTheDocument();
  });
});
