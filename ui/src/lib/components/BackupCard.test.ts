// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

const backup = vi.hoisted(() => ({ run: vi.fn() }));
vi.mock("#lib/settings/backup.ts", async () => {
  const real = await vi.importActual<typeof import("#lib/settings/backup.ts")>(
    "#lib/settings/backup.ts",
  );
  return { ...real, backUpNow: backup.run };
});

import BackupCard from "./BackupCard.svelte";

afterEach(() => {
  cleanup();
  backup.run.mockReset();
});

describe("BackupCard", () => {
  it("says what the file holds and does not hold before anything is pressed", () => {
    render(BackupCard);
    expect(screen.getByText(/holds no token, password or key/)).toBeTruthy();
    expect(screen.queryByRole("status")).toBeNull();
    expect(backup.run).not.toHaveBeenCalled();
  });

  it("is busy while the backup is made, then names the saved file and its size", async () => {
    let finish: (value: unknown) => void = () => undefined;
    backup.run.mockReturnValue(new Promise((resolve) => (finish = resolve)));
    render(BackupCard);
    const button = screen.getByTestId("back-up-now") as HTMLButtonElement;
    await fireEvent.click(button);
    expect(button.disabled).toBe(true);
    expect(button.textContent).toContain("Backing up");
    finish({ ok: true, name: "b.tar.gz", bytes: 2048 });
    expect(await screen.findByTestId("backup-done")).toBeTruthy();
    expect(screen.getByTestId("backup-done").textContent).toContain(
      "Saved b.tar.gz (2 KB)",
    );
    expect(button.disabled).toBe(false);
  });

  it("shows the reason on a failure and clears it on the next try", async () => {
    backup.run.mockResolvedValueOnce({
      ok: false,
      reason: "The disk is full.",
    });
    render(BackupCard);
    await fireEvent.click(screen.getByTestId("back-up-now"));
    expect((await screen.findByTestId("backup-problem")).textContent).toContain(
      "The disk is full.",
    );
    backup.run.mockResolvedValueOnce({ ok: true, name: "c.tar.gz", bytes: 5 });
    await fireEvent.click(screen.getByTestId("back-up-now"));
    expect(await screen.findByTestId("backup-done")).toBeTruthy();
    expect(screen.queryByTestId("backup-problem")).toBeNull();
  });
});
