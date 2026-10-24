// SPDX-License-Identifier: GPL-3.0-or-later
import { expect, signIn, test, watchCsp } from "./support";

// Computed-style and box-size checks of the visual identity, in place of screenshot baselines.

test.describe("the visual identity, served by the real API", () => {
  test.beforeEach(async ({ page }) => {
    await signIn(page);
  });

  test("the bundled fonts load from the app itself and are the ones in use", async ({
    page,
  }) => {
    const violations = await watchCsp(page);
    const fontFiles: string[] = [];
    page.on("response", (r) => {
      if (r.url().endsWith(".woff2")) fontFiles.push(new URL(r.url()).origin);
    });
    await page.goto("/workspaces");
    await expect(
      page.getByRole("heading", { level: 1, name: "Workspaces" }),
    ).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(() =>
          [...document.fonts]
            .filter((f) => f.status === "loaded")
            .map((f) => f.family.replaceAll('"', "")),
        ),
      )
      .toContain("Inter Variable");
    const used = await page.evaluate(() => ({
      body: getComputedStyle(document.body).fontFamily,
    }));
    expect(used.body).toMatch(/^"?Inter Variable"?,/);
    expect(fontFiles.length).toBeGreaterThan(0);
    expect(new Set(fontFiles)).toEqual(new Set([new URL(page.url()).origin]));
    expect(await violations()).toEqual([]);
  });

  test("the duck is the mark in the sidebar, at its box size, and the tab icon is the duck", async ({
    page,
  }) => {
    await page.goto("/workspaces");
    const duck = page.locator(".sidebar .brand svg");
    await expect(duck).toBeVisible();
    const box = await duck.boundingBox();
    expect(box?.width).toBe(28);
    expect(box?.height).toBe(28);
    await expect(duck).toHaveAttribute("aria-hidden", "true");
    await expect(page.locator(".sidebar .brand")).toContainText("Puddle");
    const icon = page.locator('link[rel="icon"]');
    await expect(icon).toHaveAttribute("type", "image/svg+xml");
    const response = await page.request.get(
      (await icon.getAttribute("href")) as string,
    );
    expect(response.ok()).toBe(true);
    expect(await response.text()).toContain("<svg");
  });

  for (const scheme of ["light", "dark"] as const) {
    test(`the Pond tokens apply in ${scheme} mode, and the duck follows its tokens`, async ({
      page,
    }) => {
      await page.emulateMedia({ colorScheme: scheme });
      await page.goto("/workspaces");
      await expect(page.locator(".sidebar .brand svg")).toBeVisible();
      const style = await page.evaluate(() => {
        const root = getComputedStyle(document.documentElement);
        return {
          bg: getComputedStyle(document.body).backgroundColor,
          accent: root.getPropertyValue("--color-accent").trim(),
          body: getComputedStyle(
            document.querySelector(".sidebar .brand .body") as Element,
          ).fill,
        };
      });
      expect(style.bg).toBe(
        scheme === "light" ? "rgb(244, 247, 249)" : "rgb(11, 23, 29)",
      );
      expect(style.accent).toMatch(/^#/);
      expect(style.body).toBe("rgb(246, 196, 49)");
    });
  }
});
