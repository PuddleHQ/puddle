// SPDX-License-Identifier: GPL-3.0-or-later
// The Identities screens against the real API on the fixture backend, scenario `git-identities`:
// two identities (Work: gh account tijs-work for acme and acme-labs plus a Git credential for
// dev.azure.com/contoso that is signed out; Personal: gh account tijs-demo for the rest of
// github.com), three workspaces, and what the fake credential host answers. Each test has a
// backend of its own, reset before it. Waits are on conditions, never on time.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeStops, watchCsp, staysEmpty } from "./support";

async function open(page: Page, backend: Backend, path = "/identities") {
  await backend.control.reset("git-identities");
  await backend.signIn(page);
  await page.goto(path);
}

const notices = (page: Page) => page.getByRole("region", { name: "Notices" });

/** Emits until the page shows the notice: the event stream may connect after the page loaded. */
async function raise(
  backend: Backend,
  page: Page,
  event: Record<string, unknown>,
  text: string,
) {
  await expect(async () => {
    await backend.control.emit(event);
    await expect(notices(page)).toContainText(text, { timeout: 500 });
  }).toPass();
}

const SIGN_IN_NEEDED = {
  type: "credential_sign_in_needed",
  host: "dev.azure.com",
  source: "Git credential for https://dev.azure.com/contoso",
};

const row = (page: Page, label: string) =>
  page
    .locator("[data-identity-id]")
    .filter({ has: page.getByRole("heading", { name: label }) });

test("lists the identities with author, credentials, default and use; the sidebar marks the section", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  await open(page, backend);
  await expect(
    page.getByRole("heading", { level: 1, name: "Identities" }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "Identities", exact: true }).first(),
  ).toHaveAttribute("aria-current", "page");
  const work = row(page, "Work");
  await expect(work).toContainText("Default");
  await expect(work).toContainText("Tijs Work <tijs@acme.example>");
  await expect(work).toContainText(
    "gh · tijs-work · github.com: acme, acme-labs",
  );
  await expect(work).toContainText(
    "Git Credential Manager · dev.azure.com/contoso",
  );
  await expect(work).toContainText("Used by 2 workspaces");
  await expect(work).toContainText("Not tested");
  const personal = row(page, "Personal");
  await expect(personal).not.toContainText("Default");
  await expect(personal).toContainText("the rest of github.com");
  await expect(page).toHaveTitle("Identities - Puddle");
  await staysEmpty(page, csp);
});

test("a credential's last read survives a reload, from a push and a repository list", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  const work = row(page, "Work");
  const personal = row(page, "Personal");
  await expect(personal).toContainText("Not tested");
  await backend.control.script("pushes_read_credentials");
  await page.reload();
  await expect(personal).toContainText("OK");
  await expect(work).toContainText("Sign in needed");
  await expect(personal).not.toContainText("Not tested");
  // The detail page says when and what asked, and Test still reads again.
  await page.getByRole("link", { name: "Personal", exact: true }).click();
  const cred = page.locator("[data-credential]").first();
  await expect(cred.getByTestId("checked")).toContainText(
    /Checked 4 minutes ago by a workspace's request/,
  );
  await cred.getByRole("button", { name: /^Test / }).click();
  await expect(cred.getByTestId("checked")).toContainText(/by a test/);
  await page.goto("/identities");
  await page.getByRole("link", { name: "Work", exact: true }).click();
  const azure = page.locator("[data-credential]").nth(1);
  await expect(azure).toContainText("Sign in needed");
  await expect(azure.getByTestId("checked")).toContainText(
    /by a repository list/,
  );
  // Test all stays: it reads every credential and the answers replace the record.
  await page.goto("/identities");
  await page.getByRole("button", { name: "Test all" }).click();
  await expect(row(page, "Personal")).toContainText("OK");
});

test("Settings points to the Identities section", async ({ page, backend }) => {
  await open(page, backend, "/settings");
  await page.getByRole("link", { name: "Manage identities" }).click();
  await expect(page).toHaveURL(/\/identities$/);
});

test("Test all reads each credential and shows who needs to sign in, never a value", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await page.getByRole("button", { name: "Test all" }).click();
  await expect(row(page, "Personal")).toContainText("OK");
  await expect(row(page, "Work")).toContainText("Sign in needed");
  // eslint-disable-next-line no-restricted-syntax -- absence: the settled state is asserted above, and a poll of a negative passes on its first read too
  expect(await page.content()).not.toMatch(/ghp_|token=/);
});

test("signing in happens on a click: the dialog shows the code and the credential reads afterwards", async ({
  page,
  backend,
}) => {
  await open(page, backend, "/identities/1");
  await expect(
    page.getByRole("heading", { level: 1, name: "Work" }),
  ).toBeVisible();
  const azure = page.locator(
    '[data-credential^="Git credential for https://dev.azure.com"]',
  );
  await azure.getByRole("button", { name: /^Test / }).click();
  await expect(azure).toContainText("Sign in needed");
  await expect(azure).toContainText("not signed in");
  // Nothing signed in by itself.
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await azure.getByRole("button", { name: /^Sign in to / }).click();
  const dialog = page.getByRole("dialog", { name: "Sign in" });
  await expect(dialog.getByTestId("sign-in-code")).toHaveText("ABCD-1234");
  await expect(
    dialog.getByRole("link", { name: "https://github.com/login/device" }),
  ).toHaveAttribute("rel", /noopener/);
  // The fake finishes the sign-in at once; the dialog sees it on its next check.
  await expect(dialog).toContainText("Signed in.", { timeout: 15_000 });
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(azure).toContainText("OK");
});

test("adds an identity from an account found on this computer", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await page.getByRole("button", { name: "Add identity" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Add identity" });
  await dialog.getByLabel("Identity name").fill("Side project");
  await dialog.getByLabel("Author name").fill("Tijs S");
  await dialog.getByLabel("Author email").fill("tijs@side.example");
  await dialog.getByRole("button", { name: "Add a credential" }).click();
  await dialog.getByLabel(/tijs-demo on github.com \(GitHub CLI\)/).check();
  await expect(dialog.getByLabel("The rest of github.com")).toBeChecked();
  await dialog
    .getByLabel(/Owners or organisations it covers on github.com/)
    .fill("side-org");
  await dialog.getByRole("button", { name: "Add credential" }).click();
  await expect(dialog).toContainText(
    "gh · tijs-demo · github.com: side-org and the rest of github.com",
  );
  await dialog.getByRole("button", { name: "Add identity" }).click();
  await expect(dialog).toBeHidden();
  const made = row(page, "Side project");
  await expect(made).toContainText("Tijs S <tijs@side.example>");
  await expect(made).not.toContainText("Default");
  await expect(page.getByText("Added Side project.")).toBeVisible();
});

test("fills the author and offers the organisations from the account, and shows the host's words as text", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await page.getByRole("button", { name: "Add identity" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Add identity" });
  await dialog.getByLabel("Identity name").fill("Demo");
  await dialog.getByLabel("Author name").fill("Tijs V");
  await dialog.getByRole("button", { name: "Add a credential" }).click();
  await dialog.getByLabel(/tijs-demo on github.com/).check();
  // The empty address is filled; the typed name stays until the button is pressed.
  await expect(dialog.getByLabel("Author email")).toHaveValue(
    "4242+tijs-demo@users.noreply.github.com",
  );
  await expect(dialog.getByLabel("Author name")).toHaveValue("Tijs V");
  await dialog
    .getByRole("button", { name: "Use this account's name and address" })
    .click();
  await expect(dialog.getByLabel("Author name")).toHaveValue("Tijs Demo");
  await dialog.getByRole("button", { name: "demo-labs" }).click();
  await expect(dialog.getByLabel(/Owners or organisations/)).toHaveValue(
    "demo-labs",
  );
  await expect(dialog).toContainText(
    "Organisations that require single sign-on may be missing.",
  );
  await dialog.getByRole("button", { name: "Add credential" }).click();
  await dialog.getByRole("button", { name: "Add identity" }).click();
  await expect(dialog).toBeHidden();
  await expect(row(page, "Demo")).toContainText(
    "Tijs Demo <4242+tijs-demo@users.noreply.github.com>",
  );
});

test("a profile the host could not read shows its problem as plain text and never blocks adding the credential", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await page.getByRole("button", { name: "Add identity" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Add identity" });
  await dialog.getByRole("button", { name: "Add a credential" }).click();
  await dialog.getByLabel(/tijs-old on github.com/).check();
  await expect(dialog).toContainText(
    "GitHub did not accept the login <b>tijs-old</b>.",
  );
  await expect(dialog.getByLabel("Author name")).toHaveValue("");
  await dialog.getByRole("button", { name: "Add credential" }).click();
  await expect(dialog).toContainText("gh · tijs-old · github.com");
});

test("an Azure DevOps account says its author must be typed and offers its organisation", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await page.getByRole("button", { name: "Add identity" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Add identity" });
  await dialog.getByRole("button", { name: "Add a credential" }).click();
  await dialog.getByLabel(/tijs@contoso.example on dev.azure.com/).check();
  await expect(dialog).toContainText(
    "type the author name and address yourself",
  );
  await expect(dialog.getByRole("button", { name: "contoso" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(dialog.getByLabel("Author name")).toHaveValue("");
});

test("a pasted token is write-only: it is kept by id and never appears in any answer", async ({
  page,
  backend,
  request,
}) => {
  const secret = "ghp_e2eSecretValue0123456789";
  await open(page, backend);
  await page.getByRole("button", { name: "Add identity" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Add identity" });
  await dialog.getByLabel("Identity name").fill("Token user");
  await dialog.getByLabel("Author name").fill("T");
  await dialog.getByLabel("Author email").fill("t@example.com");
  await dialog.getByRole("button", { name: "Add a credential" }).click();
  await dialog.getByLabel("Paste a token").check();
  await expect(dialog.getByLabel("Token", { exact: true })).toHaveAttribute(
    "type",
    "password",
  );
  await dialog.getByLabel("Git host").fill("dev.azure.com");
  await dialog.getByLabel("Azure DevOps organisation").fill("fabrikam");
  await dialog.getByLabel("Token", { exact: true }).fill(secret);
  await dialog.getByLabel(/Owners or organisations/).fill("fabrikam");
  await dialog.getByLabel(/The rest of dev.azure.com/).uncheck();
  await dialog.getByRole("button", { name: "Add credential" }).click();
  await expect(dialog).toContainText(
    "Pasted token · fabrikam · dev.azure.com: fabrikam",
  );
  await dialog.getByRole("button", { name: "Add identity" }).click();
  await expect(dialog).toBeHidden();
  await expect(row(page, "Token user")).toContainText("Pasted token");
  const answer = await request.get("/api/identities", {
    headers: { Authorization: `Bearer ${backend.token}` },
  });
  const text = await answer.text();
  expect(text).toContain('"kind":"stored"');
  expect(text).not.toContain(secret);
  // eslint-disable-next-line no-restricted-syntax -- absence: the settled state is asserted above, and a poll of a negative passes on its first read too
  expect(await page.content()).not.toContain(secret);
});

const PASTED = "ghp_e2ePastedLookup0123456789";

/** Opens the editor on a pasted GitHub token, typed and not yet looked up. */
async function pasteToken(page: Page, backend: Backend, token = PASTED) {
  await open(page, backend);
  await page.getByRole("button", { name: "Add identity" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Add identity" });
  await dialog.getByRole("button", { name: "Add a credential" }).click();
  await dialog.getByLabel("Paste a token").check();
  await dialog.getByLabel("Git host").fill("github.com");
  await dialog.getByLabel("Token", { exact: true }).fill(token);
  return dialog;
}

test("looking up a pasted token keeps it, fills the author, and adding the credential reuses that entry", async ({
  page,
  backend,
  request,
}) => {
  const dialog = await pasteToken(page, backend);
  expect((await backend.control.state()).kept_tokens).toEqual([]);
  await dialog.getByRole("button", { name: "Look up this account" }).click();
  await expect(dialog).toContainText("tijs-pasted on github.com: Tijs Pasted");
  await expect(dialog.getByLabel("Author email")).toHaveValue(
    "7+tijs-pasted@users.noreply.github.com",
  );
  await expect(
    dialog.getByRole("button", { name: "pasted-org" }),
  ).toBeVisible();
  const [kept] = (await backend.control.state()).kept_tokens;
  expect(kept).toBeTruthy();
  await dialog.getByLabel("Identity name").fill("Pasted");
  await dialog.getByRole("button", { name: "Add credential" }).click();
  await dialog.getByRole("button", { name: "Add identity" }).click();
  await expect(dialog).toBeHidden();
  await expect(row(page, "Pasted")).toContainText(
    "Tijs Pasted <7+tijs-pasted@users.noreply.github.com>",
  );
  expect((await backend.control.state()).kept_tokens).toEqual([kept]);
  const answer = await request.get("/api/identities", {
    headers: { Authorization: `Bearer ${backend.token}` },
  });
  const text = await answer.text();
  expect(text).toContain(`"id":"${kept}"`);
  expect(text).not.toContain(PASTED);
});

test("a changed token replaces the kept entry, and an unadded kept token is removed when the editor closes", async ({
  page,
  backend,
}) => {
  const dialog = await pasteToken(page, backend);
  const lookUp = dialog.getByRole("button", { name: "Look up this account" });
  await lookUp.click();
  await expect(dialog).toContainText("tijs-pasted on github.com");
  const [first] = (await backend.control.state()).kept_tokens;
  await dialog
    .getByLabel("Token", { exact: true })
    .fill("ghp_someOtherToken0123456789");
  await lookUp.click();
  await expect(dialog).toContainText(
    "the fake host has no profile for this credential",
  );
  await expect
    .poll(async () => (await backend.control.state()).kept_tokens)
    .toHaveLength(1);
  const [second] = (await backend.control.state()).kept_tokens;
  expect(second).not.toBe(first);
  await dialog.getByRole("button", { name: "Cancel" }).first().click();
  await expect(dialog.getByLabel("Paste a token")).toBeHidden();
  await expect
    .poll(async () => (await backend.control.state()).kept_tokens)
    .toEqual([]);
});

test("checks a form before it asks and keeps focus on the first problem", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await page.getByRole("button", { name: "Add identity" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Add identity" });
  await dialog.getByRole("button", { name: "Add identity" }).click();
  await expect(dialog.getByRole("alert")).toContainText(
    "Give the identity a name",
  );
  await expect(dialog.getByLabel("Identity name")).toBeFocused();
  await dialog.getByLabel("Identity name").fill("work");
  await dialog.getByRole("button", { name: "Add identity" }).click();
  await expect(dialog.getByRole("alert")).toContainText("already called work");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

test("sets the default, moves an identity and deletes one, naming the workspaces that lose it", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await row(page, "Personal")
    .getByRole("button", { name: "Set Personal as default" })
    .click();
  await expect(row(page, "Personal")).toContainText("Default");
  await expect(row(page, "Work")).not.toContainText("Default");
  await row(page, "Personal")
    .getByRole("button", { name: "Move Personal up" })
    .click();
  await expect(page.locator("[data-identity-id]").first()).toContainText(
    "Personal",
  );

  await row(page, "Work").getByRole("button", { name: "Delete Work" }).click();
  const confirm = page.getByRole("alertdialog");
  await expect(confirm).toContainText(
    /(docs-site, web-shop|web-shop, docs-site) will lose it/,
  );
  await expect(confirm.getByRole("button", { name: "Cancel" })).toBeFocused();
  await confirm.getByRole("button", { name: "Delete Work" }).click();
  await expect(
    page.getByText(
      /^Deleted Work; it was taken off (docs-site, web-shop|web-shop, docs-site)\.$/,
    ),
  ).toBeVisible();
  await expect(row(page, "Work")).toHaveCount(0);
});

test("the sign-in notice leads to the credential's page and goes when the credential reads", async ({
  page,
  backend,
}) => {
  await open(page, backend, "/workspaces");
  const notice = notices(page);
  await raise(backend, page, SIGN_IN_NEEDED, "Sign-in needed");
  await expect(notice).toContainText(
    "Git credential for https://dev.azure.com/contoso",
  );
  await notice.getByRole("link", { name: "Sign in" }).click();
  await expect(page).toHaveURL(/\/identities\/1$/);
  const azure = page.locator(
    '[data-credential^="Git credential for https://dev.azure.com"]',
  );
  await expect(azure).toContainText("Sign in needed");
  // The user signs in (the fake host makes the credential readable), and a check says so.
  await backend.control.step({
    do: "credential_readable",
    readable: true,
    source: {
      kind: "git_credential",
      host: "dev.azure.com",
      path: "contoso",
      username: null,
    },
  });
  await azure.getByRole("button", { name: /^Test / }).click();
  await expect(azure).toContainText("OK");
  await expect(notice).not.toContainText("Sign-in needed");
});

test("keyboard only: add, move and delete without a pointer", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await page.getByRole("button", { name: "Add identity" }).first().focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "Add identity" });
  await expect(dialog.getByLabel("Identity name")).toBeFocused();
  await page.keyboard.type("Keys");
  await page.keyboard.press("Tab");
  await page.keyboard.type("K");
  await page.keyboard.press("Tab");
  await page.keyboard.type("k@example.com");
  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  await expect(row(page, "Keys")).toBeVisible();

  await row(page, "Keys").getByRole("button", { name: "Move Keys up" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("[data-identity-id]").nth(1)).toContainText("Keys");
  await expect(
    page.getByRole("button", { name: "Move Keys up" }),
  ).toBeFocused();

  await row(page, "Keys").getByRole("button", { name: "Delete Keys" }).focus();
  await page.keyboard.press("Enter");
  const confirm = page.getByRole("alertdialog");
  await expect(confirm.getByRole("button", { name: "Cancel" })).toBeFocused();
  await page.keyboard.press("Tab");
  await page.keyboard.press("Enter");
  await expect(row(page, "Keys")).toHaveCount(0);
  await expect(
    page.getByRole("heading", { level: 1, name: "Identities" }),
  ).toBeFocused();
});

// The states are scanned at most two to a test: a scan costs seconds on a loaded host.
const stops = axeStops([
  ["list", "list with statuses"],
  ["add identity with the editor", "add identity with an error"],
  ["identity", "sign-in dialog"],
  ["with the sign-in notice"],
]);
for (const scheme of ["light", "dark"] as const) {
  for (const group of stops.groups) {
    test(`axe finds nothing in ${scheme}: ${group.join(", ")}`, async ({
      page,
      backend,
    }) => {
      const scan = stops.scanner(group);
      await page.emulateMedia({ colorScheme: scheme });
      const csp = await watchCsp(page);
      await open(page, backend);
      await expect(row(page, "Work")).toBeVisible();
      await scan(page, "list");

      await page.getByRole("button", { name: "Test all" }).click();
      await expect(row(page, "Work")).toContainText("Sign in needed");
      await scan(page, "list with statuses");

      await page.getByRole("button", { name: "Add identity" }).first().click();
      const dialog = page.getByRole("dialog", { name: "Add identity" });
      await dialog.getByRole("button", { name: "Add a credential" }).click();
      await expect(dialog.getByLabel(/tijs-demo/)).toBeVisible();
      await scan(page, "add identity with the editor");
      await dialog.getByLabel("Paste a token").check();
      await dialog.getByRole("button", { name: "Add credential" }).click();
      await expect(dialog.getByRole("alert")).toBeVisible();
      await scan(page, "add identity with an error");
      await page.keyboard.press("Escape");

      await page.goto("/identities/1");
      await expect(
        page.getByRole("heading", { level: 1, name: "Work" }),
      ).toBeVisible();
      await page
        .getByRole("button", { name: /^Test / })
        .first()
        .click();
      await scan(page, "identity");
      const azure = page.locator(
        '[data-credential^="Git credential for https://dev.azure.com"]',
      );
      await azure.getByRole("button", { name: /^Test / }).click();
      await azure.getByRole("button", { name: /^Sign in to / }).click();
      await expect(page.getByTestId("sign-in-code")).toBeVisible();
      await scan(page, "sign-in dialog");
      await page.keyboard.press("Escape");

      await raise(backend, page, SIGN_IN_NEEDED, "Sign-in needed");
      await scan(page, "with the sign-in notice");
      await staysEmpty(page, csp);
      scan.done();
    });
  }
}
