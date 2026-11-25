// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({
  status: 500,
  error: null as { message: string; detail?: string } | null,
}));
vi.mock("$app/state", () => ({
  page: {
    get status() {
      return state.status;
    },
    get error() {
      return state.error;
    },
  },
}));

import ErrorPage from "./+error.svelte";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const DETAIL =
  "TypeError: Cannot assign to read only property 'toString' of object '#<Object>'\n  at x.js:9:7650";

function show(status: number, error: { message: string; detail?: string }) {
  state.status = status;
  state.error = error;
  render(ErrorPage);
}

describe("the error page", () => {
  it("shows what was thrown under a collapsed Details, with a copy button", async () => {
    const writeText = vi.fn(async () => {});
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    show(500, { message: "Internal Error", detail: DETAIL });
    expect(
      screen.getByRole("heading", { name: "Something went wrong" }),
    ).toBeTruthy();
    expect(screen.getByText("Internal Error")).toBeTruthy();
    const details = screen.getByText("Details").closest("details");
    expect(details?.open).toBe(false);
    expect(details?.querySelector("pre")?.textContent).toBe(DETAIL);
    await fireEvent.click(screen.getByRole("button", { name: "Copy details" }));
    expect(writeText).toHaveBeenCalledWith(DETAIL);
    expect(screen.getByRole("status").textContent).toBe("Copied.");
  });

  it("says how to copy by hand when the clipboard refuses", async () => {
    vi.stubGlobal("navigator", {
      clipboard: {
        writeText: vi.fn(async () => Promise.reject(new Error("no"))),
      },
    });
    show(500, { message: "Internal Error", detail: DETAIL });
    await fireEvent.click(screen.getByRole("button", { name: "Copy details" }));
    expect(screen.getByRole("status").textContent).toBe(
      "Couldn't copy them. Select the text and copy it by hand.",
    );
  });

  it("has no Details for a missing page or an error without them", () => {
    show(404, { message: "Not Found", detail: DETAIL });
    expect(
      screen.getByRole("heading", { name: "Page not found" }),
    ).toBeTruthy();
    expect(screen.queryByText("Details")).toBeNull();
    // A page that is not there offers the way home, an error does not.
    expect(
      screen.getByRole("link", { name: "Go to Workspaces" }),
    ).toHaveAttribute("href", "/workspaces");
    cleanup();
    show(500, { message: "Internal Error" });
    expect(screen.queryByText("Details")).toBeNull();
    expect(screen.queryByRole("link", { name: "Go to Workspaces" })).toBeNull();
  });
});
