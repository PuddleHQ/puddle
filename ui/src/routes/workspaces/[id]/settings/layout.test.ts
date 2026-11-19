// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, render, screen, within } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

const url = vi.hoisted(() => ({ pathname: "/workspaces/w-demo/settings" }));
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(`http://127.0.0.1${url.pathname}`);
    },
    get params() {
      return { id: "w-demo" };
    },
  },
}));

import Layout from "./+layout.svelte";

afterEach(cleanup);

function list() {
  return within(screen.getByRole("navigation", { name: "Workspace settings" }));
}

describe("the list beside the settings pages", () => {
  it("names the five pages and marks the table as current on the Settings address", () => {
    url.pathname = "/workspaces/w-demo/settings";
    render(Layout, { children: (() => {}) as never });
    expect(
      list()
        .getAllByRole("link")
        .map((l) => l.textContent),
    ).toEqual(["Settings", "Environment", "Shell init", "SSH agent", "Ports"]);
    expect(list().getByRole("link", { name: "Settings" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(list().getByRole("link", { name: "Ports" })).toHaveAttribute(
      "href",
      "/workspaces/w-demo/settings/ports",
    );
  });

  it("marks the page it is on", () => {
    url.pathname = "/workspaces/w-demo/settings/shell-init";
    render(Layout, { children: (() => {}) as never });
    expect(list().getByRole("link", { name: "Shell init" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(list().getByRole("link", { name: "Settings" })).not.toHaveAttribute(
      "aria-current",
    );
  });
});
