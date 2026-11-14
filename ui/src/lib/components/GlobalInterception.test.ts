// SPDX-License-Identifier: AGPL-3.0-or-later
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

import { INTERCEPTION_NOTICE_KEY } from "#lib/notify/interception.ts";
import { notices } from "#lib/notify/notices.svelte.ts";
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

  it("says why the setting could not be read, and shows no controls", async () => {
    api.refuse = true;
    render(GlobalInterception);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Couldn't read the interception setting.",
    );
    expect(screen.queryByLabelText("Which hosts to decrypt")).toBeNull();
  });

  it("puts the learning switch back and says why when it could not be saved", async () => {
    render(GlobalInterception);
    const box = await screen.findByLabelText("Learn from rejections");
    api.refuse = true;
    await fireEvent.click(box);
    await waitFor(() => expect(screen.getByRole("alert")).toBeInTheDocument());
    expect(box).toBeChecked();
  });

  it("says the service did not answer when it is down", async () => {
    render(GlobalInterception);
    const select = await screen.findByLabelText("Which hosts to decrypt");
    api.down = true;
    await fireEvent.change(select, { target: { value: "all" } });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /answer|running|reach/i,
    );
  });

  it("shows nothing when the service is down at the start", async () => {
    api.down = true;
    render(GlobalInterception);
    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });

  it("settles the one-time notice when a mode is chosen", async () => {
    notices.add({ key: INTERCEPTION_NOTICE_KEY, tone: "info", title: "New" });
    render(GlobalInterception);
    const select = await screen.findByLabelText("Which hosts to decrypt");
    await fireEvent.change(select, { target: { value: "all" } });
    await screen.findByText("Interception mode saved.");
    expect(notices.items).toEqual([]);
  });
});
