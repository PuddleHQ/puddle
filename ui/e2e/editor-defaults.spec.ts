// SPDX-License-Identifier: AGPL-3.0-or-later
// The editor defaults against the real API on the fixture backend (scenario `lived-in`): set on
// Settings > Editor, inherited by a workspace with the source named, overridden there for one
// field, and handed back with "Use global". Each step reads the saved value back from the API.
import type { APIRequestContext, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";

type Editor = {
  colour_theme: string | null;
  font_size: number | null;
  extensions: string[] | null;
  keybindings: string | null;
};

const globalEditor = async (request: APIRequestContext, backend: Backend) => {
  const r = await request.get("/api/settings", {
    headers: { Authorization: `Bearer ${backend.token}` },
  });
  return ((await r.json()) as { workspace_defaults: { editor: Editor } })
    .workspace_defaults.editor;
};

const workspaceEditor = async (
  request: APIRequestContext,
  backend: Backend,
) => {
  const r = await request.get("/api/settings/workspaces/web-shop", {
    headers: { Authorization: `Bearer ${backend.token}` },
  });
  return (await r.json()) as {
    overrides: { editor: Editor };
    effective: { editor: Record<keyof Editor, { source: string }> };
  };
};

const row = (page: Page, name: string) =>
  page.locator(`[data-setting="${name}"]`);

test("defaults set in Settings reach a workspace with their source, and a workspace overrides one field and hands it back", async ({
  page,
  backend,
  request,
}) => {
  await backend.control.reset("lived-in");
  await backend.signIn(page);
  await page.goto("/settings/editor");
  await expect(
    page.getByRole("heading", { level: 2, name: "Editor" }),
  ).toBeVisible();

  // Global: a theme and a font size, each saved with its own button.
  await page.getByLabel("Editor colour theme").fill("Default Dark Modern");
  await page.getByRole("button", { name: "Save Editor colour theme" }).click();
  await expect
    .poll(async () => (await globalEditor(request, backend)).colour_theme)
    .toBe("Default Dark Modern");
  await page.getByLabel("Font size (pixels)").fill("16");
  await page.getByRole("button", { name: "Save Font size (pixels)" }).click();
  await expect
    .poll(async () => (await globalEditor(request, backend)).font_size)
    .toBe(16);
  await page
    .getByLabel("Extensions to install")
    .fill("Rust-Lang.Rust-Analyzer\nesbenp.prettier-vscode");
  await page
    .getByRole("button", { name: "Save Extensions to install" })
    .click();
  await expect
    .poll(async () => (await globalEditor(request, backend)).extensions)
    .toEqual(["esbenp.prettier-vscode", "rust-lang.rust-analyzer"]);
  expect((await globalEditor(request, backend)).colour_theme).toBe(
    "Default Dark Modern",
  );

  // A value the API would refuse is held back with the reason.
  await page.getByLabel("Font size (pixels)").fill("99");
  await expect(page.getByRole("alert")).toContainText("8 to 40");
  await expect(
    page.getByRole("button", { name: "Save Font size (pixels)" }),
  ).toBeDisabled();
  await expectNoAxeViolations(page, "/settings/editor");

  // The workspace inherits all of it and says so.
  await page.goto("/workspaces/web-shop/settings");
  await expect(page.getByLabel("Editor colour theme")).toHaveValue(
    "Default Dark Modern",
  );
  await expect(row(page, "editor-theme")).toContainText("Global setting");
  await expect(page.getByLabel("Font size (pixels)")).toHaveValue("16");
  await expect(row(page, "editor-keybindings")).toContainText(
    "Puddle's default",
  );
  await expect(
    page.getByRole("button", {
      name: /Use the global setting again for Colour/,
    }),
  ).toHaveCount(0);

  // One workspace field is overridden; the others still follow the global value.
  await page.getByLabel("Editor colour theme").fill("Solarized Dark");
  await page.getByRole("button", { name: "Save Editor colour theme" }).click();
  await expect
    .poll(
      async () => (await workspaceEditor(request, backend)).overrides.editor,
    )
    .toMatchObject({ colour_theme: "Solarized Dark", font_size: null });
  await expect(row(page, "editor-theme")).toContainText("This workspace");
  await expect(row(page, "editor-font-size")).toContainText("Global setting");
  expect(
    (await workspaceEditor(request, backend)).effective.editor.colour_theme
      .source,
  ).toBe("workspace");
  await expectNoAxeViolations(page, "/workspaces/web-shop/settings");

  // An empty list overrides the global extensions with none.
  await page.getByLabel("Extensions to install").fill("");
  await page
    .getByRole("button", { name: "Save Extensions to install" })
    .click();
  await expect
    .poll(
      async () =>
        (await workspaceEditor(request, backend)).overrides.editor.extensions,
    )
    .toEqual([]);

  // "Use global" hands the field back.
  await page
    .getByRole("button", {
      name: "Use the global setting again for Editor colour theme",
    })
    .click();
  await expect
    .poll(
      async () =>
        (await workspaceEditor(request, backend)).overrides.editor.colour_theme,
    )
    .toBeNull();
  await expect(page.getByLabel("Editor colour theme")).toHaveValue(
    "Default Dark Modern",
  );
  await expect(row(page, "editor-theme")).toContainText("Global setting");
});
