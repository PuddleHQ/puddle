// SPDX-License-Identifier: GPL-3.0-or-later
// The colour palettes against the real API on the fixture backend: chosen in Settings, kept by
// Puddle, applied live over the light or dark theme without a Content-Security-Policy violation,
// and accessible (axe's colour contrast included) in every palette and both themes.
// Waits are on conditions, never on time.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeViolations, watchCsp } from "./support";

const PALETTES = ["pond", "dusk", "meadow", "ember", "graphite"] as const;

const saved = (page: Page) =>
  page.waitForResponse(
    (r) => r.request().method() === "PUT" && r.url().endsWith("/api/settings"),
  );

async function open(page: Page, backend: Backend, path = "/settings") {
  await backend.signIn(page);
  await page.goto(path);
}

async function save(page: Page) {
  const done = saved(page);
  await page.getByRole("button", { name: "Save" }).click();
  await done;
}

const accent = (page: Page) =>
  page.evaluate(() =>
    getComputedStyle(document.documentElement)
      .getPropertyValue("--color-accent")
      .trim(),
  );

test.describe("the palette setting", () => {
  test("is Pond by default, changes the whole app live, and Puddle keeps it", async ({
    page,
    backend,
    request,
  }) => {
    const csp = await watchCsp(page);
    await open(page, backend);
    await expect(page.getByLabel("Colours", { exact: true })).toHaveValue(
      "pond",
    );
    await expect(page.locator("html")).not.toHaveAttribute(
      "data-palette",
      /.*/,
    );
    const pond = await accent(page);

    await page.getByLabel("Colours", { exact: true }).selectOption("meadow");
    await expect(page.locator("html")).toHaveAttribute(
      "data-palette",
      "meadow",
    );
    await expect.poll(() => accent(page)).not.toBe(pond);
    await save(page);

    const view = await request.get("/api/settings", {
      headers: { Authorization: `Bearer ${backend.token}` },
    });
    expect(
      ((await view.json()) as { ui: { palette: string } }).ui.palette,
    ).toBe("meadow");

    // A new launch has a new origin: nothing in the page's storage survives, Puddle remembers.
    await page.evaluate(() => localStorage.clear());
    await page.goto("/settings");
    await expect(page.getByLabel("Colours", { exact: true })).toHaveValue(
      "meadow",
    );
    await expect(page.locator("html")).toHaveAttribute(
      "data-palette",
      "meadow",
    );

    await page.getByLabel("Colours", { exact: true }).selectOption("pond");
    await save(page);
    await expect(page.locator("html")).not.toHaveAttribute(
      "data-palette",
      /.*/,
    );
    await expect.poll(() => accent(page)).toBe(pond);
    await expect.poll(() => csp()).toEqual([]);
  });

  test("Custom takes the user's accent, keeps it, and warns when a pick is hard to read", async ({
    page,
    backend,
    request,
  }) => {
    const csp = await watchCsp(page);
    await open(page, backend);
    await page.getByLabel("Theme", { exact: true }).selectOption("light");
    await page.getByLabel("Colours", { exact: true }).selectOption("custom");
    await expect(page.getByText(/hard to read/)).toBeHidden();

    await page.locator("#set-colour-accent").fill("#7a1fa2");
    await expect.poll(() => accent(page)).toContain("#7a1fa2");
    await expect(page.getByText(/hard to read/)).toBeHidden();

    await page.locator("#set-colour-text").fill("#cccccc");
    const warning = page
      .getByRole("status")
      .filter({ hasText: /hard to read/ });
    await expect(warning).toContainText("Text on the background has");
    await expect(warning).toContainText("in the light theme");

    await page.getByRole("button", { name: "Reset text" }).click();
    await expect(page.getByText(/hard to read/)).toBeHidden();
    await save(page);

    const view = await request.get("/api/settings", {
      headers: { Authorization: `Bearer ${backend.token}` },
    });
    const { ui } = (await view.json()) as {
      ui: { palette: string; custom_accent: string; custom_text: string };
    };
    expect(ui).toMatchObject({
      palette: "custom",
      custom_accent: "#7a1fa2",
      custom_text: null,
    });

    await page.evaluate(() => localStorage.clear());
    await page.goto("/settings");
    await expect(page.locator("#set-colour-accent")).toHaveValue("#7a1fa2");
    await expect.poll(() => accent(page)).toContain("#7a1fa2");
    await expect.poll(() => csp()).toEqual([]);
  });

  for (const palette of [...PALETTES, "custom"] as const) {
    for (const theme of ["light", "dark"] as const) {
      test(`${palette} in the ${theme} theme: axe finds nothing on the settings screen`, async ({
        page,
        backend,
      }) => {
        await open(page, backend);
        await page.getByLabel("Theme", { exact: true }).selectOption(theme);
        await page.getByLabel("Colours", { exact: true }).selectOption(palette);
        if (palette === "custom") {
          // A set that passes AA: a darker accent and ink on a pale ground, or the reverse.
          const ground = theme === "light" ? "#fbf8f1" : "#14110c";
          const ink = theme === "light" ? "#1c1710" : "#f1ebdd";
          const tint = theme === "light" ? "#7a3d00" : "#ffb866";
          await page.locator("#set-colour-background").fill(ground);
          await page.locator("#set-colour-surface").fill(ground);
          await page.locator("#set-colour-text").fill(ink);
          await page.locator("#set-colour-accent").fill(tint);
          await expect(page.getByText(/hard to read/)).toBeHidden();
        }
        await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
        await expect
          .poll(() => axeViolations(page), { message: `${palette} ${theme}` })
          .toEqual([]);
      });
    }
  }
});
