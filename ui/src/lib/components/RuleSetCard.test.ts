// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { builtIn, mine, trackerEntries } from "#lib/testing/fake-rule-sets.ts";
import type { RuleSet } from "#lib/rules/sets.ts";
import RuleSetCard from "./RuleSetCard.svelte";

afterEach(cleanup);

function mount(set: RuleSet) {
  const props = {
    set,
    workspaces: ["api", "web"],
    onSwitch: vi.fn(),
    onAddEntry: vi.fn(),
    onRename: vi.fn(),
    onDelete: vi.fn(),
    onDeleteEntry: vi.fn(),
  };
  render(RuleSetCard, { props } as never);
  return props;
}

describe("a set's colour", () => {
  it("is a chip on the heading, with the name written out", () => {
    mount(mine(4, { name: "Work", fill: { kind: "solid", color: "#c04f7a" } }));
    const heading = screen.getByRole("heading", { name: "Work" });
    expect(heading.firstElementChild).toHaveStyle("--set-fill: #c04f7a");
    expect(heading).toHaveTextContent("Work");
  });

  it("is the built-in set's own", () => {
    mount(builtIn("github", { name: "GitHub" }));
    expect(
      screen.getByRole("heading", { name: "GitHub" }).firstElementChild,
    ).toHaveStyle("--set-fill: #2f80c9");
  });
});

describe("a built-in set", () => {
  it("says what it is, that it ships off, and has no actions of yours", () => {
    mount(builtIn("github", { name: "GitHub", changed_at: 0 }));
    expect(screen.getByRole("article", { name: "GitHub" })).toBeInTheDocument();
    expect(screen.getByText("Built in")).toBeInTheDocument();
    expect(screen.getByText("builtin:github")).toBeInTheDocument();
    expect(screen.getByText(/Updated by Puddle/)).toBeInTheDocument();
    expect(
      screen.getByRole("switch", { name: "On for every workspace" }),
    ).not.toBeChecked();
    expect(
      screen.getByText("Off (built-in sets ship off)"),
    ).toBeInTheDocument();
    expect(screen.getByText("1 entry")).toBeInTheDocument();
    expect(screen.getByText("main host")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Delete/ })).toBeNull();
  });

  it("asks the page to switch it, and leaves the box as it was until the answer", async () => {
    const props = mount(builtIn("github"));
    const box = screen.getByRole("switch", { name: "On for every workspace" });
    await fireEvent.click(box);
    expect(props.onSwitch).toHaveBeenCalledWith(props.set, null, true);
    expect(box).not.toBeChecked();
  });

  it("switches for one workspace, checking the name first", async () => {
    const props = mount(builtIn("github"));
    await fireEvent.click(screen.getByRole("button", { name: "On there" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Name the workspace");
    expect(props.onSwitch).not.toHaveBeenCalled();
    await fireEvent.input(screen.getByLabelText("Workspace name"), {
      target: { value: "api" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Off there" }));
    expect(props.onSwitch).toHaveBeenCalledWith(props.set, "api", false);
    // Enter in the field submits nothing by itself: the buttons say on or off.
    await fireEvent.submit(
      screen.getByRole("form", { name: "Switch github for one workspace" }),
    );
    expect(props.onSwitch).toHaveBeenCalledTimes(1);
  });

  it("lists the workspaces that switch it, each with a way back", async () => {
    const props = mount(
      builtIn("github", {
        global: true,
        overrides: [{ workspace: "api" as never, enabled: false }],
      }),
    );
    expect(
      screen.getByRole("list", { name: "Workspaces that switch github" }),
    ).toHaveTextContent("Off in api");
    await fireEvent.click(
      screen.getByRole("button", { name: "Follow every workspace" }),
    );
    expect(props.onSwitch).toHaveBeenCalledWith(props.set, "api", null);
    await fireEvent.click(
      screen.getByRole("button", { name: "Back to the default (off)" }),
    );
    expect(props.onSwitch).toHaveBeenCalledWith(props.set, null, null);
  });
});

describe("a set you made", () => {
  it("has its own actions and entries you can delete", async () => {
    const set = mine(4, {
      name: "Client X",
      description: "their tenant",
      entries: [
        {
          pattern: ".azure.com",
          pattern_kind: "suffix",
          effect: "allow",
          note: "",
          enabled: true,
          rule_id: 9,
          expires_at: null,
          precedence: 5,
        },
      ],
    });
    const props = mount(set);
    expect(screen.getByText("Yours")).toBeInTheDocument();
    expect(screen.getByText("their tenant")).toBeInTheDocument();
    expect(
      screen.getByRole("switch", { name: "On for every workspace" }),
    ).toBeChecked();
    await fireEvent.click(screen.getByRole("button", { name: "Add entry" }));
    await fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    await fireEvent.click(screen.getByRole("button", { name: "Delete set" }));
    await fireEvent.click(
      screen.getByRole("button", { name: "Delete *.azure.com from Client X" }),
    );
    expect(props.onAddEntry).toHaveBeenCalledWith(set);
    expect(props.onRename).toHaveBeenCalledWith(set);
    expect(props.onDelete).toHaveBeenCalledWith(set);
    expect(props.onDeleteEntry).toHaveBeenCalledWith(set, set.entries[0]);
  });

  it("says how to fill an empty set", () => {
    mount(mine(4));
    expect(screen.getByText("0 entries")).toBeInTheDocument();
    expect(
      screen.getByText(/approve a request into this set/),
    ).toBeInTheDocument();
  });
});

describe("what a set does", () => {
  const entry = (pattern: string, effect: "allow" | "deny") => ({
    pattern,
    pattern_kind: "exact" as const,
    effect,
    note: "",
    enabled: true,
    rule_id: null,
    expires_at: null,
    precedence: 0,
  });

  it("says Allows for a set of allows", () => {
    mount(builtIn("github"));
    expect(screen.getByText("Allows")).toBeInTheDocument();
  });

  it("says Blocks for a built-in deny set and gives each entry a Deny label", () => {
    mount(
      builtIn("trackers", {
        entries: [
          entry("a.tracker.example", "deny"),
          entry("b.example", "deny"),
        ],
      }),
    );
    expect(screen.getByText("Blocks")).toBeInTheDocument();
    expect(screen.getAllByText("Deny")).toHaveLength(2);
    expect(screen.queryByText("Allow")).toBeNull();
    expect(screen.queryByRole("button", { name: /^Delete/ })).toBeNull();
  });

  it("says Allows and blocks for a mixed set and labels every entry", () => {
    mount(
      mine(5, {
        entries: [entry("ok.example", "allow"), entry("ads.example", "deny")],
      }),
    );
    expect(screen.getByText("Allows and blocks")).toBeInTheDocument();
    expect(screen.getAllByText("Allow")).toHaveLength(1);
    expect(screen.getAllByText("Deny")).toHaveLength(1);
  });

  it("says nothing about an empty set", () => {
    mount(mine(6));
    expect(screen.queryByText(/^Allows|^Blocks/)).toBeNull();
  });
});

describe("a big built-in set", () => {
  function big(extra: Partial<RuleSet> = {}) {
    return builtIn("trackers", {
      entries: trackerEntries(200),
      entry_count: 450,
      ...extra,
    });
  }

  it("lists the first entries and says how many there are", () => {
    render(RuleSetCard, {
      props: { ...baseProps(), set: big(), loadEntries: vi.fn() },
    } as never);
    expect(screen.getByText("450 entries")).toBeInTheDocument();
    expect(screen.getAllByText(/tracker\.example/)).toHaveLength(200);
    expect(screen.getByText("Showing 200 of 450.")).toBeInTheDocument();
  });

  it("searches and shows only the matches", async () => {
    const loadEntries = vi.fn(async () => ({
      entries: trackerEntries(3),
      total: 3,
    }));
    render(RuleSetCard, {
      props: { ...baseProps(), set: big(), loadEntries },
    } as never);
    await fireEvent.input(screen.getByLabelText("Search entries"), {
      target: { value: "t1" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Search" }));
    await screen.findByText("Showing 3 of 3 matches.");
    expect(loadEntries).toHaveBeenCalledWith(expect.anything(), "t1", 0);
    expect(screen.getAllByText(/tracker\.example/)).toHaveLength(3);
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });

  it("shows more from where the list ends", async () => {
    const loadEntries = vi.fn(async () => ({
      entries: trackerEntries(250).slice(200),
      total: 450,
    }));
    render(RuleSetCard, {
      props: { ...baseProps(), set: big(), loadEntries },
    } as never);
    await fireEvent.click(screen.getByRole("button", { name: "Show more" }));
    await screen.findByText("Showing 250 of 450 matches.");
    expect(loadEntries).toHaveBeenCalledWith(expect.anything(), "", 200);
  });

  it("says when the entries can't be read", async () => {
    const loadEntries = vi.fn(async () => {
      throw new Error("down");
    });
    render(RuleSetCard, {
      props: { ...baseProps(), set: big(), loadEntries },
    } as never);
    await fireEvent.click(screen.getByRole("button", { name: "Show more" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Puddle couldn't read the entries.",
    );
  });

  it("has no search on a small set", () => {
    render(RuleSetCard, {
      props: { ...baseProps(), set: builtIn("github"), loadEntries: vi.fn() },
    } as never);
    expect(screen.queryByLabelText("Search entries")).toBeNull();
  });
});

function baseProps() {
  return {
    workspaces: [],
    onSwitch: vi.fn(),
    onAddEntry: vi.fn(),
    onRename: vi.fn(),
    onDelete: vi.fn(),
    onDeleteEntry: vi.fn(),
  };
}
