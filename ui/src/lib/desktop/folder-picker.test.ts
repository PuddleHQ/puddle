// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";
import { inDesktopApp, pickFolder } from "./folder-picker.ts";

const dialog = vi.hoisted(() => ({ open: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => dialog);

beforeEach(() => {
  dialog.open.mockReset();
});

describe("inDesktopApp", () => {
  it("is true only where the app's bridge exists", () => {
    expect(inDesktopApp({})).toBe(false);
    expect(inDesktopApp({ __TAURI_INTERNALS__: {} })).toBe(true);
  });
});

describe("pickFolder", () => {
  it("asks for one directory and returns its path", async () => {
    dialog.open.mockResolvedValue("/home/me/project");
    await expect(pickFolder("Choose the checkout")).resolves.toBe(
      "/home/me/project",
    );
    expect(dialog.open).toHaveBeenCalledWith({
      directory: true,
      multiple: false,
      title: "Choose the checkout",
    });
  });

  it("returns null when the dialog is cancelled", async () => {
    dialog.open.mockResolvedValue(null);
    await expect(pickFolder("x")).resolves.toBeNull();
  });

  it("passes a failure to open the dialog on", async () => {
    dialog.open.mockRejectedValue(new Error("no dialog"));
    await expect(pickFolder("x")).rejects.toThrow("no dialog");
  });
});
