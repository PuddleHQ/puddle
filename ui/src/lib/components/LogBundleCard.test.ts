// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

const logs = vi.hoisted(() => ({ run: vi.fn() }));
vi.mock("#lib/settings/logs.ts", () => ({ exportLogBundle: logs.run }));

import LogBundleCard from "./LogBundleCard.svelte";

afterEach(() => {
  cleanup();
  logs.run.mockReset();
});

describe("LogBundleCard", () => {
  it("says what the file cuts out before anything is pressed", () => {
    render(LogBundleCard);
    expect(screen.getByText(/Tokens, passwords, cookies/)).toBeTruthy();
    expect(screen.queryByRole("status")).toBeNull();
    expect(logs.run).not.toHaveBeenCalled();
  });

  it("is busy while the bundle is made, then names the saved file and its size", async () => {
    let finish: (value: unknown) => void = () => undefined;
    logs.run.mockReturnValue(new Promise((resolve) => (finish = resolve)));
    render(LogBundleCard);
    const button = screen.getByTestId("export-log-bundle") as HTMLButtonElement;
    await fireEvent.click(button);
    expect(button.disabled).toBe(true);
    expect(button.textContent).toContain("Exporting");
    finish({ ok: true, name: "l.tar.gz", bytes: 2048 });
    expect(
      (await screen.findByTestId("log-bundle-done")).textContent,
    ).toContain("Saved l.tar.gz (2 KB)");
    expect(button.disabled).toBe(false);
  });

  it("shows the reason on a failure and clears it on the next try", async () => {
    logs.run.mockResolvedValueOnce({ ok: false, reason: "The disk is full." });
    render(LogBundleCard);
    await fireEvent.click(screen.getByTestId("export-log-bundle"));
    expect(
      (await screen.findByTestId("log-bundle-problem")).textContent,
    ).toContain("The disk is full.");
    logs.run.mockResolvedValueOnce({ ok: true, name: "m.tar.gz", bytes: 5 });
    await fireEvent.click(screen.getByTestId("export-log-bundle"));
    expect(await screen.findByTestId("log-bundle-done")).toBeTruthy();
    expect(screen.queryByTestId("log-bundle-problem")).toBeNull();
  });

  it("drops the saved-file line when the next try fails", async () => {
    logs.run.mockResolvedValueOnce({ ok: true, name: "a.tar.gz", bytes: 5 });
    render(LogBundleCard);
    await fireEvent.click(screen.getByTestId("export-log-bundle"));
    expect(await screen.findByTestId("log-bundle-done")).toBeTruthy();
    logs.run.mockResolvedValueOnce({ ok: false, reason: "No space." });
    await fireEvent.click(screen.getByTestId("export-log-bundle"));
    expect(await screen.findByTestId("log-bundle-problem")).toBeTruthy();
    expect(screen.queryByTestId("log-bundle-done")).toBeNull();
  });
});
