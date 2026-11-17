// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { UiError } from "#lib/errors/listing.ts";

const api = vi.hoisted(() => ({ GET: vi.fn() }));
vi.mock("#lib/api/client.ts", () => ({ api }));

import Page from "./+page.svelte";

const AT = Date.UTC(2026, 10, 17, 21, 35);

function error(over: Partial<UiError> = {}): UiError {
  return {
    at: AT,
    kind: "page",
    message: "TypeError: Cannot assign to read only property 'toString'",
    stack: "  at render (/_app/immutable/nodes/31.js:9:7650)",
    route: "/workspaces/abc/terminal",
    build: "1700000000000",
    ...over,
  };
}

function answer(errors: UiError[], dropped = 0) {
  api.GET.mockResolvedValue({ data: { errors, dropped } });
}

beforeEach(() => {
  api.GET.mockReset();
});
afterEach(cleanup);

describe("app errors page", () => {
  it("lists each error with where it happened, its build, its message and its stack", async () => {
    answer(
      [
        error(),
        error({
          kind: "unhandled_rejection",
          stack: null,
          route: null,
          build: null,
          message: "Thrown: gone",
        }),
      ],
      2,
    );
    render(Page);
    expect(api.GET).toHaveBeenCalledWith("/api/ui-errors");
    const list = await screen.findByRole("list", { name: "App errors" });
    const items = list.querySelectorAll("li");
    expect(items).toHaveLength(2);
    expect(items[0]).toHaveTextContent("Error page shown");
    expect(items[0]).toHaveTextContent("/workspaces/abc/terminal");
    expect(items[0]).toHaveTextContent("build 1700000000000");
    expect(items[0]).toHaveTextContent("read only property 'toString'");
    expect(items[0]?.querySelector("details pre")).toHaveTextContent(
      "at render (/_app/immutable/nodes/31.js:9:7650)",
    );
    expect(items[1]).toHaveTextContent("Unhandled promise");
    expect(items[1]?.querySelector("details")).toBeNull();
    expect(screen.getByText(/2 more errors were left out/)).toBeInTheDocument();
    expect(document.activeElement).toBe(
      screen.getByRole("heading", { level: 1, name: "App errors" }),
    );
  });

  it("says so when there are none, and when the service does not answer", async () => {
    answer([]);
    render(Page);
    expect(
      await screen.findByText("No errors since Puddle started."),
    ).toBeInTheDocument();
    cleanup();
    api.GET.mockRejectedValue(new TypeError("Failed to fetch"));
    render(Page);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /isn't answering/,
    );
  });

  it("copies an error as text and reads the list again on Refresh", async () => {
    answer([error()]);
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
    render(Page);
    await fireEvent.click(await screen.findByRole("button", { name: /^Copy/ }));
    expect(writeText).toHaveBeenCalledOnce();
    const text = writeText.mock.calls[0]?.[0 as never] as unknown as string;
    expect(text).toContain("Error page shown at ");
    expect(text).toContain("Page: /workspaces/abc/terminal");
    expect(text).toContain("Build: 1700000000000");
    expect(text).toContain("at render (/_app/immutable/nodes/31.js:9:7650)");
    expect(await screen.findByText("Copied.")).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(api.GET).toHaveBeenCalledTimes(2);
  });
});
