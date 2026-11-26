// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

const nav = vi.hoisted(() => ({ goto: vi.fn() }));
vi.mock("$app/navigation", () => ({ goto: nav.goto }));

import Redirect from "./+page.svelte";

afterEach(() => {
  cleanup();
  nav.goto.mockClear();
  window.location.hash = "";
});

describe("/settings", () => {
  it("opens Appearance, replacing itself in the history", async () => {
    render(Redirect);
    await vi.waitFor(() =>
      expect(nav.goto).toHaveBeenCalledWith("/settings/appearance", {
        replaceState: true,
      }),
    );
  });

  it("opens the page an old section anchor now lives on", async () => {
    window.location.hash = "#s-disk";
    render(Redirect);
    await vi.waitFor(() =>
      expect(nav.goto).toHaveBeenCalledWith(
        "/settings/this-computer#set-disk-warn",
        { replaceState: true },
      ),
    );
  });
});
