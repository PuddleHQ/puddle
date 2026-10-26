// SPDX-License-Identifier: GPL-3.0-or-later
// The App icon section of Settings: built-in choices, the user's own image, and Save.
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = await vi.hoisted(async () => {
  const mod = await import("#lib/testing/fake-settings.ts");
  return new mod.FakeSettings();
});
vi.mock("#lib/api/client.ts", () => ({ api }));
vi.mock("$app/navigation", () => ({ beforeNavigate: () => undefined }));

import { MAX_ICON_BYTES } from "#lib/settings/app-icon.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import Page from "./+page.svelte";

beforeEach(() => {
  api.reset();
  globalSettings.view = null;
  globalSettings.consents = null;
  globalSettings.status = "loading";
  globalSettings.reason = null;
  localStorage.clear();
});
afterEach(cleanup);

async function open() {
  render(Page);
  await screen.findByRole("heading", { level: 2, name: "App icon" });
  await screen.findByRole("radio", { name: "Duck on a pond" });
}

const radio = (name: string) =>
  screen.getByRole("radio", { name }) as HTMLInputElement;

async function give(file: File) {
  const input = screen.getByTestId("icon-file") as HTMLInputElement;
  await fireEvent.change(input, { target: { files: [file] } });
}

const png = (bytes = 8) =>
  new File([new Uint8Array(bytes).fill(1)], "mine.png", { type: "image/png" });

describe("the App icon section", () => {
  it("has its own place in the list of sections, after Appearance", async () => {
    await open();
    const nav = screen.getByRole("navigation", { name: "Settings sections" });
    const names = within(nav)
      .getAllByRole("link")
      .map((a) => a.textContent);
    expect(names.indexOf("App icon")).toBe(names.indexOf("Appearance") + 1);
  });

  it("offers the four ducks, the duck chosen, and your own image off until there is one", async () => {
    await open();
    expect(screen.getAllByRole("radio")).toHaveLength(5);
    for (const name of [
      "Duck",
      "Duck on a pond",
      "Duck at night",
      "Duck in the sun",
      "Your image",
    ]) {
      expect(radio(name)).toBeTruthy();
    }
    expect(radio("Duck").checked).toBe(true);
    expect(radio("Your image").disabled).toBe(true);
    expect(
      screen.getByRole("button", { name: "Choose an image…" }),
    ).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Remove your image" }),
    ).toBeNull();
  });

  it("holds the choice until Save, then stores it", async () => {
    await open();
    await fireEvent.click(radio("Duck at night"));
    expect(radio("Duck at night").checked).toBe(true);
    expect(api.ui.app_icon).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText("Settings saved.");
    expect(api.ui.app_icon).toBe("duck_night");
  });

  it("drops the edit when the duck is chosen back, and on Discard", async () => {
    await open();
    await fireEvent.click(radio("Duck in the sun"));
    await fireEvent.click(radio("Duck"));
    expect(
      (screen.getByRole("button", { name: "Save" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    await fireEvent.click(radio("Duck on a pond"));
    await fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(radio("Duck").checked).toBe(true);
  });

  it("stores a picked image at once, chooses it as an unsaved edit and keeps it on Save", async () => {
    await open();
    await give(png());
    await screen.findByText("Your image is ready. Save to use it.");
    expect(api.customIcon).toBe("own-png");
    expect(radio("Your image").checked).toBe(true);
    expect(radio("Your image").disabled).toBe(false);
    expect(api.ui.app_icon).toBeNull();
    expect(
      screen.getByRole("button", { name: "Replace your image…" }),
    ).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText("Settings saved.");
    expect(api.ui.app_icon).toBe("custom");
  });

  it("says why the service refused an image, and chooses nothing", async () => {
    await open();
    await give(new File([], "empty.png", { type: "image/png" }));
    expect((await screen.findByRole("alert")).textContent).toContain(
      "The file is empty.",
    );
    expect(radio("Duck").checked).toBe(true);
    expect(api.customIcon).toBeNull();
  });

  it("refuses a file over 8 MB without sending it", async () => {
    await open();
    const big = png();
    Object.defineProperty(big, "size", { value: MAX_ICON_BYTES + 1 });
    await give(big);
    expect((await screen.findByRole("alert")).textContent).toContain(
      "larger than 8 MB",
    );
    expect(api.calls).not.toContain("PUT /api/app-icons/custom");
  });

  it("says the service did not answer when it is down", async () => {
    await open();
    api.down = true;
    await give(png());
    expect((await screen.findByRole("alert")).textContent).toContain(
      "isn't answering",
    );
  });

  it("removes your image once nothing saved uses it, and goes back to the saved icon", async () => {
    api.customIcon = "own-png";
    await open();
    await fireEvent.click(radio("Your image"));
    await fireEvent.click(
      screen.getByRole("button", { name: "Remove your image" }),
    );
    await screen.findByText("Your image is removed.");
    expect(api.customIcon).toBeNull();
    expect(radio("Duck").checked).toBe(true);
    expect(radio("Your image").disabled).toBe(true);
  });

  it("does not offer to remove the image while it is the saved icon", async () => {
    api.customIcon = "own-png";
    api.ui.app_icon = "custom";
    await open();
    expect(radio("Your image").checked).toBe(true);
    const remove = screen.getByRole("button", {
      name: "Remove your image",
    }) as HTMLButtonElement;
    expect(remove.disabled).toBe(true);
    expect(
      screen.getByText(/choose another icon and save before removing it/),
    ).toBeTruthy();
  });

  it("shows what the service says when removing is refused", async () => {
    api.customIcon = "own-png";
    await open();
    api.refuse.set("DELETE /api/app-icons/custom", {
      status: 409,
      error: "conflict",
      message: "your own image is the icon in use",
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Remove your image" }),
    );
    expect((await screen.findByRole("alert")).textContent).toContain("in use");
    expect(api.customIcon).toBe("own-png");
  });

  it("says when the icons cannot be loaded, and the rest of Settings still works", async () => {
    api.refuse.set("GET /api/app-icons", {
      status: 500,
      error: "internal",
      message: "internal error; see Puddle's log",
    });
    render(Page);
    const alert = await screen.findByText(/The icons could not be loaded/);
    expect(alert.textContent).toContain("Internal error");
    await waitFor(() => expect(screen.getByLabelText("Theme")).toBeTruthy());
  });
});
