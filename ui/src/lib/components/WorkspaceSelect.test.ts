// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { workspace as fakeWorkspace } from "#lib/testing/fake-workspaces.ts";
import WorkspaceSelect from "./WorkspaceSelect.svelte";

beforeEach(() => {
  workspaces.list = [
    { ...fakeWorkspace("Shop"), id: "g07jxx5q" },
    { ...fakeWorkspace("api"), id: "h19kyy6r" },
  ];
});
afterEach(() => {
  cleanup();
  workspaces.list = [];
});

const options = () =>
  screen
    .getAllByRole("option")
    .map((o) => [o.textContent, (o as HTMLOptionElement).value]);

describe("the workspace dropdown", () => {
  it("shows names and keeps the id as the value", () => {
    render(WorkspaceSelect, { props: { id: "w" } });
    expect(options()).toEqual([
      ["Choose a workspace", ""],
      ["api", "h19kyy6r"],
      ["Shop", "g07jxx5q"],
    ]);
  });
  it("adds the id only next to a name another workspace shares", () => {
    workspaces.list = [
      { ...fakeWorkspace("Shop"), id: "g07jxx5q" },
      { ...fakeWorkspace("Shop"), id: "h19kyy6r" },
    ];
    render(WorkspaceSelect, { props: { id: "w" } });
    expect(options().map((o) => o[0])).toEqual([
      "Choose a workspace",
      "Shop (g07jxx5q)",
      "Shop (h19kyy6r)",
    ]);
  });
  it("lists an id the live list lacks (a deleted workspace) as it is", () => {
    render(WorkspaceSelect, { props: { id: "w", known: ["gone0001"] } });
    expect(options()).toContainEqual(["gone0001", "gone0001"]);
  });
  it("takes the choice as the id", async () => {
    render(WorkspaceSelect, { props: { id: "w", value: "" } });
    const select = screen.getByRole("combobox");
    await fireEvent.change(select, { target: { value: "g07jxx5q" } });
    expect(select).toHaveDisplayValue("Shop");
  });
});
