// SPDX-License-Identifier: GPL-3.0-or-later
// Forgetting a login Puddle kept, from the notice that says it can't be used (fixture scenario
// `lived-in`). The whole path runs by keyboard; the store's refusal shows its way out in the
// dialog. Layout is checked by computed styles and box sizes, never by pictures.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { boxSide, expectNoAxeViolations, insideBy } from "./support";

const notices = (page: Page) => page.getByRole("region", { name: "Notices" });
const forgetButton = (page: Page) =>
  notices(page).getByRole("button", { name: "Forget Claude Code login" });
const dialog = (page: Page) => page.getByRole("alertdialog");

const capturedOff = {
  type: "login_problem",
  workspace: "web-shop",
  service: "Claude Code",
  kind: "capture_off",
};

async function open(page: Page, backend: Backend) {
  await backend.control.reset("lived-in");
  await backend.signIn(page);
  await page.goto("/inbox");
  await expect(notices(page)).toBeAttached();
  // The stream may connect after the page loaded: emit until the notice shows.
  await expect(async () => {
    await backend.control.emit(capturedOff);
    await expect(forgetButton(page)).toBeVisible({ timeout: 500 });
  }).toPass();
}

test("a kept login is forgotten by keyboard, after a confirmation that says what goes", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await expectNoAxeViolations(page);

  await forgetButton(page).focus();
  await page.keyboard.press("Enter");
  await expect(dialog(page)).toBeVisible();
  await expect(dialog(page)).toHaveAccessibleName(
    "Forget the Claude Code login in web-shop?",
  );
  await expect(dialog(page)).toContainText("credential store");
  await expect(dialog(page)).toContainText("asks you to sign in again");
  // The safe choice has the focus; the confirm button is one Tab away.
  await expect(
    dialog(page).getByRole("button", { name: "Cancel" }),
  ).toBeFocused();
  await expectNoAxeViolations(page);
  await page.keyboard.press("Tab");
  await expect(
    dialog(page).getByRole("button", { name: "Forget login" }),
  ).toBeFocused();
  await page.keyboard.press("Enter");

  await expect(dialog(page)).toHaveCount(0);
  await expect(notices(page).getByText(/can't be used/)).toHaveCount(0);
  // The notice and its button are gone: the focus is on the page, not lost.
  await expect(page.locator("#main")).toBeFocused();
  await expectNoAxeViolations(page);
});

test("Escape leaves the login and its notice alone", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await forgetButton(page).focus();
  await page.keyboard.press("Enter");
  await expect(dialog(page)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog(page)).toHaveCount(0);
  await expect(forgetButton(page)).toBeVisible();
});

test("a store that refuses shows its way out in the dialog and keeps the notice, and the next try works", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await backend.control.step({ do: "login_store_refuses", refuses: true });
  await forgetButton(page).focus();
  await page.keyboard.press("Enter");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Enter");

  const why = dialog(page).getByRole("alert");
  await expect(why).toContainText(
    "credential store did not delete a saved login; unlock it or start it and ask again",
  );
  await expect(dialog(page)).toBeVisible();
  await expect(forgetButton(page)).toHaveCount(1);
  await expectNoAxeViolations(page);

  // Layout: the message has its own border colour next to the dialog's, nothing leaves the
  // dialog, and every button is a target of at least 24 by 24 CSS pixels (WCAG 2.5.8).
  const look = await dialog(page).evaluate((el) => {
    const message = el.querySelector<HTMLElement>(".confirm-error")!;
    return {
      message: getComputedStyle(message).borderTopColor,
      dialog: getComputedStyle(el).borderTopColor,
    };
  });
  expect(look.message).not.toBe(look.dialog);
  const view = page.viewportSize()!;
  await expect
    .poll(async () => {
      const edge = await dialog(page).boundingBox();
      return edge
        ? Math.min(
            edge.x,
            view.width - edge.x - edge.width,
            view.height - edge.y - edge.height,
          )
        : -1;
    })
    .toBeGreaterThanOrEqual(0);
  for (const control of [
    dialog(page).getByRole("button", { name: "Cancel" }),
    dialog(page).getByRole("button", { name: "Forget login" }),
  ]) {
    await boxSide(control, "width").toBeGreaterThanOrEqual(24);
    await boxSide(control, "height").toBeGreaterThanOrEqual(24);
    await insideBy(dialog(page), control).toBeGreaterThanOrEqual(0);
  }

  // The store answers again: the same dialog, one more press.
  await backend.control.step({ do: "login_store_refuses", refuses: false });
  await dialog(page).getByRole("button", { name: "Forget login" }).focus();
  await page.keyboard.press("Enter");
  await expect(dialog(page)).toHaveCount(0);
  await expect(notices(page).getByText(/can't be used/)).toHaveCount(0);
});

test("the Forget button of a notice is a target big enough to hit and does not overflow the notice", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  const notice = page.locator(".notice", {
    has: page.getByRole("button", { name: "Forget Claude Code login" }),
  });
  await boxSide(forgetButton(page), "width").toBeGreaterThanOrEqual(24);
  await boxSide(forgetButton(page), "height").toBeGreaterThanOrEqual(24);
  await insideBy(notice, forgetButton(page)).toBeGreaterThanOrEqual(0);
  // The warning notice's border carries the warning colour; its Forget button is an ordinary button.
  const colours = await notice.evaluate((el) => ({
    border: getComputedStyle(el).borderTopColor,
    plain: getComputedStyle(el.querySelector("button.dismiss")!).borderTopColor,
  }));
  expect(colours.border).not.toBe(colours.plain);
});
