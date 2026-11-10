// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = await vi.hoisted(async () => {
  const mod = await import("#lib/testing/fake-interception.ts");
  return new mod.FakeInterception();
});
vi.mock("#lib/api/client.ts", () => ({ api }));

import GlobalInterception from "./GlobalInterception.svelte";

beforeEach(() => api.reset());
afterEach(cleanup);

describe("the global interception setting", () => {
  it("shows the mode in force, says it has not been chosen and shows the empty curated list", async () => {
    render(GlobalInterception);
    const select = await screen.findByLabelText("Which hosts to decrypt");
    expect(select).toHaveValue("known_only");
    expect(screen.getByText(/You haven't chosen yet/)).toBeInTheDocument();
    expect(screen.getByText("System managed")).toBeInTheDocument();
    expect(screen.getByText(/The list is empty for now/)).toBeInTheDocument();
  });

  it("lists the curated names read-only", async () => {
    api.view = { ...api.view, curated: ["pinned.example", "other.example"] };
    render(GlobalInterception);
    await screen.findByText("pinned.example");
    expect(screen.getByText("other.example")).toBeInTheDocument();
  });

  it("saves a mode and the learning switch at once", async () => {
    render(GlobalInterception);
    const select = await screen.findByLabelText("Which hosts to decrypt");
    await fireEvent.change(select, { target: { value: "all_except_curated" } });
    await screen.findByText("Interception mode saved.");
    await fireEvent.click(screen.getByLabelText("Learn from rejections"));
    await screen.findByText("Learning from rejections saved.");
    expect(api.bodies).toEqual([
      { mode: "all_except_curated" },
      { learn_from_rejections: false },
    ]);
  });

  it("puts the select back and says why when the save fails", async () => {
    render(GlobalInterception);
    const select = await screen.findByLabelText("Which hosts to decrypt");
    api.refuse = true;
    await fireEvent.change(select, { target: { value: "all" } });
    await waitFor(() => expect(screen.getByRole("alert")).toBeInTheDocument());
    expect(select).toHaveValue("known_only");
  });
});
