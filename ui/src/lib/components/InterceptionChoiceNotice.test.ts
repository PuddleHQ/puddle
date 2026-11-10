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

import Notice from "./InterceptionChoiceNotice.svelte";

beforeEach(() => api.reset());
afterEach(cleanup);

describe("the interception notice", () => {
  it("is offered once to an install that has not chosen and not seen it", async () => {
    render(Notice);
    expect(await screen.findByRole("status")).toHaveTextContent(
      /decrypt every HTTPS host/,
    );
    expect(
      screen.getByRole("link", { name: "Choose in Settings" }),
    ).toHaveAttribute("href", "/settings#s-net");
  });

  it("goes away for good once answered, leaving the mode as it was", async () => {
    render(Notice);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Keep it as it is" }),
    );
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
    expect(api.bodies).toEqual([{ notice_seen: true }]);
    expect(api.view.mode).toBe("known_only");
    expect(api.view.chosen).toBe(false);
  });

  it("stays away once a mode was chosen or the notice was seen", async () => {
    api.view = { ...api.view, chosen: true, notice_seen: true };
    render(Notice);
    await waitFor(() => expect(api.calls).toEqual(["GET /api/interception"]));
    expect(screen.queryByRole("status")).toBeNull();
  });
});
