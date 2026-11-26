// SPDX-License-Identifier: AGPL-3.0-or-later
// The App icon section of Settings: built-in choices, the user's own image, and how a choice applies at once.
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = await vi.hoisted(async () => {
  const mod = await import("#lib/testing/fake-settings.ts");
  return new mod.FakeSettings();
});
vi.mock("#lib/api/client.ts", () => ({ api }));
vi.mock("$app/state", () => ({
  page: {
    url: new URL("http://127.0.0.1/settings/appearance"),
    status: 200,
    error: null,
  },
}));
vi.mock("$app/navigation", () => ({ beforeNavigate: () => undefined }));

import { MAX_ICON_BYTES } from "#lib/settings/app-icon.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import SettingsHost from "#lib/testing/SettingsHost.svelte";
import Appearance from "./(pages)/appearance/+page.svelte";

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
  render(SettingsHost, { props: { pages: [Appearance] } });
  await screen.findAllByText("App icon");
  await screen.findByRole("radio", { name: "Rubber duck" });
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
  it("sits on the Appearance page, with the other looks of the app", async () => {
    await open();
    expect(
      screen.getByRole("heading", { level: 2, name: "Appearance" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Theme")).toBeInTheDocument();
    expect(screen.getAllByText("App icon").length).toBeGreaterThan(0);
  });

  it("offers the three ducks, the duck chosen, and your own image off until there is one", async () => {
    await open();
    expect(screen.getAllByRole("radio")).toHaveLength(4);
    for (const name of [
      "Duck",
      "Rubber duck",
      "Duck, front view",
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

  it("stores the choice the moment it is made, says Saved and can be undone", async () => {
    await open();
    await fireEvent.click(radio("Duck, front view"));
    await screen.findByText("Saved.");
    expect(api.ui.app_icon).toBe("duck_front");
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: /Undo/ }));
    await screen.findByText("Undone.");
    expect(radio("Duck").checked).toBe(true);
  });

  it("opens the file chooser from the button", async () => {
    await open();
    const input = screen.getByTestId("icon-file") as HTMLInputElement;
    const opened = vi.spyOn(input, "click");
    await fireEvent.click(
      screen.getByRole("button", { name: "Choose an image…" }),
    );
    expect(opened).toHaveBeenCalledOnce();
  });

  it("stores a picked image at once and chooses it", async () => {
    await open();
    await give(png());
    await screen.findByText("Your image is ready.");
    expect(api.customIcon).toBe("own-png");
    await screen.findByText("Saved.");
    expect(radio("Your image").checked).toBe(true);
    expect(radio("Your image").disabled).toBe(false);
    expect(api.ui.app_icon).toBe("custom");
    expect(
      screen.getByRole("button", { name: "Replace your image…" }),
    ).toBeTruthy();
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

  it("sends nothing when the file chooser closes without a file", async () => {
    await open();
    const input = screen.getByTestId("icon-file") as HTMLInputElement;
    await fireEvent.change(input, { target: { files: [] } });
    expect(api.calls).not.toContain("PUT /api/app-icons/custom");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(radio("Duck").checked).toBe(true);
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
      screen.getByText(/choose another icon before removing it/),
    ).toBeTruthy();
  });

  it("says so when the saved icon is your image but the image is gone", async () => {
    api.ui.app_icon = "custom";
    await open();
    expect((await screen.findByRole("alert")).textContent).toContain(
      "Your image is missing, so the duck is shown",
    );
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
    render(SettingsHost, { props: { pages: [Appearance] } });
    const alert = await screen.findByText(/The icons could not be loaded/);
    expect(alert.textContent).toContain("Internal error");
    await waitFor(() => expect(screen.getByLabelText("Theme")).toBeTruthy());
  });

  it("chooses your own image from its radio once there is one", async () => {
    api.customIcon = "own-png";
    await open();
    await fireEvent.click(radio("Your image"));
    await screen.findByText("Saved.");
    expect(api.ui.app_icon).toBe("custom");
  });
});
