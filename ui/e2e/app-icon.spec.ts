// SPDX-License-Identifier: GPL-3.0-or-later
// The app icon against the real API on the fixture backend: a built-in duck chosen and kept, an own
// image checked, resized and stored by Puddle, and the section accessible with the images shown
// without a Content-Security-Policy violation. Waits are on conditions, never on time.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeViolations, watchCsp } from "./support";

/** A 40 by 40 red PNG: valid, and smaller than the 256 pixels Puddle stores. */
const RED_PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAACgAAAAoCAYAAACM/rhtAAAAOUlEQVR42u3OMQ0AAAgDMJTMvxRcgQoSjh79W53MZyUoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKHhjATErk3UonFe7AAAAAElFTkSuQmCC",
  "base64",
);

const saved = (page: Page) =>
  page.waitForResponse(
    (r) => r.request().method() === "PUT" && r.url().endsWith("/api/settings"),
  );

async function open(page: Page, backend: Backend) {
  await backend.signIn(page);
  await page.goto("/settings");
  await expect(
    page.getByRole("radio", { name: "Duck on a pond" }),
  ).toBeVisible();
}

async function save(page: Page) {
  const done = saved(page);
  await page.getByRole("button", { name: "Save" }).click();
  await done;
}

const stored = async (
  backend: Backend,
  request: import("@playwright/test").APIRequestContext,
) => {
  const view = await request.get("/api/settings", {
    headers: { Authorization: `Bearer ${backend.token}` },
  });
  return ((await view.json()) as { ui: { app_icon: string | null } }).ui
    .app_icon;
};

test.describe("the app icon", () => {
  test("shows the ducks as pictures, starts on the duck, and Puddle keeps another choice", async ({
    page,
    backend,
    request,
  }) => {
    const csp = await watchCsp(page);
    await open(page, backend);
    await expect(
      page.getByRole("radio", { name: "Duck", exact: true }),
    ).toBeChecked();
    for (const name of [
      "Duck",
      "Duck on a pond",
      "Duck at night",
      "Duck in the sun",
    ]) {
      const picture = page
        .locator("label.choice", {
          has: page.getByRole("radio", { name, exact: true }),
        })
        .locator("img");
      await expect
        .poll(() =>
          picture.evaluate((img: HTMLImageElement) => img.naturalWidth),
        )
        .toBe(64);
    }
    await expect(
      page.getByRole("radio", { name: "Your image" }),
    ).toBeDisabled();

    await page.getByRole("radio", { name: "Duck at night" }).check();
    await save(page);
    expect(await stored(backend, request)).toBe("duck_night");

    await page.goto("/settings");
    await expect(
      page.getByRole("radio", { name: "Duck at night" }),
    ).toBeChecked();
    expect(await csp()).toEqual([]);
  });

  test("checks, resizes and stores your own image, and lets it go once another icon is saved", async ({
    page,
    backend,
    request,
  }) => {
    const csp = await watchCsp(page);
    await open(page, backend);

    await page.getByTestId("icon-file").setInputFiles({
      name: "mine.png",
      mimeType: "image/png",
      buffer: RED_PNG,
    });
    await expect(
      page.getByText("Your image is ready. Save to use it."),
    ).toBeVisible();
    const mine = page.getByRole("radio", { name: "Your image" });
    await expect(mine).toBeChecked();
    // Stored as 256 pixels square, whatever it was given as.
    const picture = page.locator("label.choice.on img");
    await expect
      .poll(() => picture.evaluate((img: HTMLImageElement) => img.naturalWidth))
      .toBe(256);
    expect(await stored(backend, request)).toBeNull();
    await save(page);
    expect(await stored(backend, request)).toBe("custom");
    await expect(
      page.getByRole("button", { name: "Remove your image" }),
    ).toBeDisabled();
    expect(await axeViolations(page)).toEqual([]);

    await page.getByRole("radio", { name: "Duck", exact: true }).check();
    await save(page);
    await page.getByRole("button", { name: "Remove your image" }).click();
    await expect(page.getByText("Your image is removed.")).toBeVisible();
    await expect(mine).toBeDisabled();
    const icons = await request.get("/api/app-icons", {
      headers: { Authorization: `Bearer ${backend.token}` },
    });
    expect(
      ((await icons.json()) as { custom_png_base64: string | null })
        .custom_png_base64,
    ).toBeNull();
    expect(await csp()).toEqual([]);
  });

  test("says why a file is not an image and keeps the icon as it was", async ({
    page,
    backend,
  }) => {
    await open(page, backend);
    await page.getByTestId("icon-file").setInputFiles({
      name: "notes.png",
      mimeType: "image/png",
      buffer: Buffer.from("this is not a picture"),
    });
    await expect(page.getByRole("alert")).toContainText(
      "not a PNG, JPEG or WebP",
    );
    await expect(
      page.getByRole("radio", { name: "Duck", exact: true }),
    ).toBeChecked();
    await expect(
      page.getByRole("radio", { name: "Your image" }),
    ).toBeDisabled();
    expect(await axeViolations(page)).toEqual([]);
  });
});
