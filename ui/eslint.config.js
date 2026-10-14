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
    files: ["scripts/**", "tools/**", "*.config.*"],
    rules: { "no-console": "off" },
  },
);
