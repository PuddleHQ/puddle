// SPDX-License-Identifier: GPL-3.0-or-later

// `expect(await read)` asserts a page value once, before the page has settled, which is how a spec
// passes on an idle machine and fails on a loaded one. ui/scripts/flake-eslint.test.ts proves the
// selector fires on the original code; ui/scripts/expect-poll-codemod.ts converts a site. It lives in its
// own file so that test loads this block and a parser, not the whole config (TypeScript, the Svelte
// compiler and every plugin: about two thirds of the load time).
/** @type {import("eslint").Linter.Config} */
export const e2eExpectAwait = {
  files: ["e2e/**/*.ts"],
  rules: {
    "no-restricted-syntax": [
      "error",
      {
        selector:
          "CallExpression[callee.name='expect'] > AwaitExpression.arguments",
        message:
          "expect(await ...) reads the page once, before it settles. Use a web-first assertion (await expect(locator).toHaveText(...)) or await expect.poll(() => read). For something that must be absent, expect.poll passes on its first empty read too: use staysEmpty(page, read) or expectNoAxeViolations(page) from support.ts. A deliberate one-shot (data held)? eslint-disable-next-line no-restricted-syntax -- <why>",
      },
    ],
  },
};
