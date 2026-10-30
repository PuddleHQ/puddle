// SPDX-License-Identifier: GPL-3.0-or-later
import js from "@eslint/js";
import svelte from "eslint-plugin-svelte";
import globals from "globals";
import ts from "typescript-eslint";

export default ts.config(
  {
    ignores: [
      "build/",
      ".svelte-kit/",
      "coverage/",
      "node_modules/",
      "playwright-report/",
      "test-results/",
      "src/lib/api/schema.d.ts",
    ],
  },
  js.configs.recommended,
  ...ts.configs.recommended,
  ...svelte.configs.recommended,
  {
    languageOptions: { globals: { ...globals.browser, ...globals.node } },
  },
  {
    files: ["**/*.svelte", "**/*.svelte.ts"],
    languageOptions: {
      parserOptions: { parser: ts.parser, extraFileExtensions: [".svelte"] },
    },
  },
  {
    // Strict where mistakes hurt: no floating promises, no unused anything.
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      "no-console": ["error", { allow: ["warn", "error"] }],
    },
  },
  {
    // The app is a static SPA served from the site root and never sets `paths.base`, so resolve()
    // would return its input unchanged. eslint-plugin-svelte 3.23.1 turned this rule on for SvelteKit 3.
    // Turn it back on if the UI is ever served from a sub-path.
    rules: { "svelte/no-navigation-without-resolve": "off" },
  },
  {
    // Counted, not failing yet (the e2e specs still have a few hundred sites): `expect(await read)` asserts a
    // page value once, before the page has settled, which is how a spec passes on an idle machine and fails
    // on a loaded one. ui/scripts/flake-eslint.test.ts proves the selector fires on the original code.
    files: ["e2e/**/*.ts"],
    rules: {
      "no-restricted-syntax": [
        "warn",
        {
          selector:
            "CallExpression[callee.name='expect'] > AwaitExpression.arguments",
          message:
            "expect(await ...) reads the page once, before it settles. Use a web-first assertion (await expect(locator).toHaveText(...)) or await expect.poll(() => read). A deliberate one-shot (data held, absence)? eslint-disable-next-line no-restricted-syntax -- <why>",
        },
      ],
    },
  },
  {
    files: ["scripts/**", "tools/**", "*.config.*"],
    rules: { "no-console": "off" },
  },
);
